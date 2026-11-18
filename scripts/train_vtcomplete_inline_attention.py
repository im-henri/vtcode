#!/usr/bin/env python3
"""Train the small language model used by VTComplete inline prediction.

A causal transformer reads text as subword tokens (byte pairs, as large
language models do), so it can write any text: names it has never seen,
comments, prose, and languages of the user's own. Before its window of the
last 1024 tokens it reads two short passages from outside that view: one from
elsewhere (earlier in the file or in a neighbouring file, chosen for the words
it shares with the text being written) and the text just below the caret,
which the prediction is to fit before. Positions are relative (rotary), so the
editor can slide the window along as the user types instead of starting over.

Training has two stages. The first teaches general prediction of the next
token on every language and on prose; some files were written by the export
in a made-up language, which teaches the model to pick up a language it does
not know from the text in front of it. The second stage tunes the model for
completion: the weights are trained as they will be stored (one byte each),
long tokens count for more because they save more typing, and the model is
trained to choose among the tokens that start with what the user typed.

Training uses PyTorch (a GPU makes it quick). The editor evaluates the stored
weights on the CPU with its own code and needs neither.

    python scripts/fetch_vtcomplete_inline_corpus.py CORPUS
    cargo build --release --example vtcomplete_inline_bench
    vtcomplete_inline_bench --export DIR CORPUS/train CORPUS/unseen
    python scripts/train_vtcomplete_inline_attention.py DIR \\
        --out assets/vtcomplete_inline_model.bin

`DIR` holds `subwords.bin`, `train.tok`, `valid.tok` and `unseen.tok` from
the export. The output is the subword merges followed by the weights as
signed bytes with one scale per matrix, the format `Network::decode` reads.
"""
import argparse
import math
import os
import queue
import struct
import subprocess
import threading
import time

import numpy as np
import torch
import torch.nn.functional as F
import torch.utils.checkpoint

LANGUAGES = 24
TOKEN = np.dtype([("token", "<u2"), ("language", "u1"), ("invented", "u1")])
MAGIC = b"VTNT6\0"
MASKED = -1e9
# Passages read before the window: `ELSEWHERE` tokens from another place, then
# `BELOW` tokens of the text under the caret. A batch shares one layout.
ELSEWHERE = 128
BELOW = 128
REFERENCE = ELSEWHERE + BELOW
# How far from the window the other place is looked for, and among how many.
FAR = 16000
CANDIDATES = 24
MARGIN = FAR + 2 * REFERENCE
# An analogous place is found by matching this many tokens before it, and
# its passage starts this many tokens before the place.
ANALOG_KEY = 8
ANALOG_BEFORE = 24


def read_subwords(blob):
    """The bytes of every token: single bytes, then one per merge, then the
    document start (no bytes)."""
    count = struct.unpack_from("<I", blob, 0)[0]
    spellings = [bytes([byte]) for byte in range(256)]
    for at in range(count):
        a, b = struct.unpack_from("<HH", blob, 4 + 4 * at)
        spellings.append(spellings[a] + spellings[b])
    return spellings + [b""]


def prefix_keys(spellings, count):
    """The first `count` bytes of each spelling as a number, or -1 for a
    spelling that is not longer than that (typing it all leaves nothing to
    complete)."""
    keys = np.full(len(spellings), -1, dtype=np.int64)
    for index, text in enumerate(spellings):
        if len(text) > count:
            keys[index] = int.from_bytes(text[:count], "little")
    return keys


def rotate(x, cos, sin):
    """Rotary positions: turn each pair of cells by the position's angle."""
    pairs = x.unflatten(-1, (-1, 2))
    a, b = pairs[..., 0], pairs[..., 1]
    return torch.stack((a * cos - b * sin, a * sin + b * cos), -1).flatten(-2)


class Model(torch.nn.Module):
    def __init__(self, vocabulary, width, layers, heads, length):
        super().__init__()
        self.vocabulary, self.width, self.layers, self.heads, self.length = vocabulary, width, layers, heads, length
        parameter = lambda *shape, scale: torch.nn.Parameter(torch.randn(*shape) * scale)
        inner = 1 / math.sqrt(width)
        # Also the output layer: a token is scored by its own description.
        self.token = parameter(vocabulary, width, scale=0.05)
        self.language = parameter(LANGUAGES, width, scale=0.05)
        # What a position belongs to: the window, elsewhere, or below.
        self.segment = parameter(3, width, scale=0.05)
        # How a window position came to stand above the text below: nothing
        # known, it stood there when reading began, or it was typed since.
        self.origin = parameter(3, width, scale=0.05)
        self.blocks = torch.nn.ModuleList()
        for _ in range(layers):
            block = torch.nn.Module()
            block.gain1 = torch.nn.Parameter(torch.ones(width))
            block.bias1 = torch.nn.Parameter(torch.zeros(width))
            block.query = parameter(width, width, scale=inner)
            block.key = parameter(width, width, scale=inner)
            block.value = parameter(width, width, scale=inner)
            block.project = parameter(width, width, scale=inner / math.sqrt(2 * layers))
            block.gain2 = torch.nn.Parameter(torch.ones(width))
            block.bias2 = torch.nn.Parameter(torch.zeros(width))
            block.expand = parameter(4 * width, width, scale=math.sqrt(2 / width))
            block.expand_bias = torch.nn.Parameter(torch.zeros(4 * width))
            block.reduce = parameter(width, 4 * width, scale=inner / math.sqrt(8 * layers))
            block.reduce_bias = torch.nn.Parameter(torch.zeros(width))
            self.blocks.append(block)
        self.final_gain = torch.nn.Parameter(torch.ones(width))
        self.final_bias = torch.nn.Parameter(torch.zeros(width))
        self.output_bias = torch.nn.Parameter(torch.zeros(vocabulary))

    @staticmethod
    def normalize(x, gain, bias):
        centered = x - x.mean(-1, keepdim=True)
        return centered * torch.rsqrt((centered * centered).mean(-1, keepdim=True) + 1e-5) * gain + bias

    def forward(self, batch, weights=None):
        """Scores over the vocabulary of every window position."""
        w = weights or dict(self.named_parameters())
        tokens, language = batch["tokens"], batch["language"]
        count, length = tokens.shape
        heads, width = self.heads, self.width
        device = tokens.device
        elsewhere, below = batch["layout"]
        reference = elsewhere + below
        index = torch.arange(length, device=device)
        segment = (index < reference).long() + ((index >= elsewhere) & (index < reference)).long()
        # Each part has its own place in the order of positions, wherever
        # it starts in this layout.
        place = torch.cat([
            torch.arange(elsewhere, device=device),
            ELSEWHERE + torch.arange(below, device=device),
            REFERENCE + torch.arange(length - reference, device=device),
        ])
        hidden = (
            w["token"][tokens]
            + w["language"][language.clamp(max=LANGUAGES - 1)][:, None, :]
            + w["segment"][segment][None]
            + w["origin"][batch["origin"]]
        )
        size = width // heads
        rate = 10000.0 ** (-torch.arange(size // 2, device=device) / (size // 2))
        angle = place[:, None].float() * rate[None, :]
        cos, sin = angle.cos(), angle.sin()
        split = lambda x: x.view(count, length, heads, size).transpose(1, 2)

        def block(hidden, layer):
            name = lambda part: w[f"blocks.{layer}.{part}"]
            normal = self.normalize(hidden, name("gain1"), name("bias1"))
            query, key, value = (split(normal @ name(part).T) for part in ("query", "key", "value"))
            query, key = rotate(query, cos, sin), rotate(key, cos, sin)
            mixed = F.scaled_dot_product_attention(query, key, value, is_causal=True)
            mixed = mixed.transpose(1, 2).reshape(count, length, width)
            hidden = hidden + mixed @ name("project").T
            normal = self.normalize(hidden, name("gain2"), name("bias2"))
            expanded = torch.relu(normal @ name("expand").T + name("expand_bias"))
            return hidden + expanded @ name("reduce").T + name("reduce_bias")

        for layer in range(self.layers):
            if torch.is_grad_enabled():
                # Recomputed on the way back instead of kept: long windows
                # would not fit in the device's memory otherwise.
                hidden = torch.utils.checkpoint.checkpoint(block, hidden, layer, use_reentrant=False)
            else:
                hidden = block(hidden, layer)
        final = self.normalize(hidden[:, reference:], w["final_gain"], w["final_bias"])
        return final @ w["token"].T + w["output_bias"]

    def loss(self, batch, weights=None, typed=None):
        """Loss and correctness of every window position's guess at the next
        token. `typed` is (positions, lengths, keys): at those positions the
        guess is also made knowing the first `lengths` bytes of the answer,
        as when the user has typed them; `keys[n]` are the vocabulary's
        prefix keys of `n` bytes."""
        scores = self.forward(batch, weights).float()
        following = batch["next"]
        losses = F.cross_entropy(scores.transpose(1, 2), following, reduction="none")
        correct = scores.argmax(-1) == following
        result = {"loss": losses, "correct": correct}
        if typed is not None:
            positions, lengths, keys = typed
            rows = scores.gather(1, positions[..., None].expand(-1, -1, scores.shape[-1]))
            answer = following.gather(1, positions)
            table = keys[lengths]  # positions' keys of their typed length, for every token
            wanted = table.gather(-1, answer[..., None])
            usable = wanted[..., 0] >= 0
            rows = rows.masked_fill(table != wanted, MASKED)
            typed_loss = torch.logsumexp(rows, -1) - rows.gather(-1, answer[..., None])[..., 0]
            result.update(
                typed_loss=typed_loss.masked_fill(~usable, 0.0),
                typed_correct=(rows.argmax(-1) == answer) & usable,
                typed_usable=usable,
            )
        return result

    def order(self):
        names = ["token", "language", "segment", "origin"]
        for layer in range(self.layers):
            names += [
                f"blocks.{layer}.{part}"
                for part in (
                    "gain1", "bias1", "query", "key", "value", "project",
                    "gain2", "bias2", "expand", "expand_bias", "reduce", "reduce_bias",
                )
            ]
        return names + ["final_gain", "final_bias", "output_bias"]

    def as_stored(self):
        """The weights the editor will see, in a form training can pass
        through: each matrix rounded to 255 levels of its largest value."""
        weights = {}
        for name, value in self.named_parameters():
            if value.ndim == 1:
                weights[name] = value
            else:
                scale = value.detach().abs().max() / 127
                rounded = torch.clamp(torch.round(value.detach() / scale), -127, 127) * scale
                weights[name] = value + (rounded - value).detach()
        return weights

    def stored(self):
        """Weights as written to disk, and the values the editor will see."""
        stored, restored = {}, {}
        for name, value in self.named_parameters():
            value = value.detach().float()
            if value.ndim == 1:
                stored[name] = (None, value.cpu().numpy().astype("<f4"))
                restored[name] = value
            else:
                scale = float(value.abs().max()) / 127 or 1.0
                compact = torch.clamp(torch.round(value / scale), -127, 127)
                stored[name] = (scale, compact.cpu().numpy().astype(np.int8))
                restored[name] = compact * np.float32(scale)
        return stored, restored


class Data:
    """A token stream with the lookups needed to cut training windows."""

    def __init__(self, tokens, wordy, device):
        self.device = device
        self.count = len(tokens)
        self.tokens = np.empty(self.count, dtype=np.uint16)
        self.language = np.empty(self.count, dtype=np.uint8)
        for at in range(0, self.count, 1 << 25):
            part = np.asarray(tokens[at : at + (1 << 25)])
            self.tokens[at : at + len(part)] = part["token"]
            self.language[at : at + len(part)] = part["language"]
        # Tokens that are words worth matching between places.
        self.wordy = wordy
        # Shares of batches given a passage from elsewhere and the text below.
        self.elsewhere_share = 0.8
        self.below_share = 0.6
        # Share of passages from elsewhere that are the analogous place, as
        # the editor's pattern predictor finds it, rather than words shared.
        self.analog_share = 0.0

    def token_ids(self, index):
        return self.tokens[index].astype(np.int64)

    def analog(self, start, end, length, random):
        """Where the analogous place's passage starts, or None: the place
        outside the window whose preceding tokens match, longest, the
        tokens before the point where reading began (the editor chooses
        the passage when it starts reading, from the caret's context, with
        the pattern predictor). The passage starts a little before the
        place, so that it holds the matched context and what followed."""
        anchor = start + length - int(random.integers(0, length // 4 + 1))
        key = self.tokens[anchor - ANALOG_KEY : anchor]
        low = max(ANALOG_KEY + ANALOG_BEFORE, start - FAR)
        high = min(self.count - ELSEWHERE - 1, end + REFERENCE + FAR)
        best, best_length = None, 3
        for first, last in ((low, start - ELSEWHERE), (end + REFERENCE + ANALOG_BEFORE, high)):
            if last - first < 8:
                continue
            region = self.tokens[first - ANALOG_KEY : last]
            # Places p (in region coordinates) with key[-1] at p - 1, and so on.
            matched = np.zeros(last - first, dtype=np.int32)
            alive = np.ones(last - first, dtype=bool)
            for back in range(1, ANALOG_KEY + 1):
                same = region[ANALOG_KEY - back : ANALOG_KEY - back + last - first] == key[-back]
                alive &= same
                matched += alive
            top = matched.max()
            if top > best_length or (top == best_length and best is not None):
                places = np.flatnonzero(matched == top) + first
                # The nearest of the best: the editor prefers what is near.
                place = places[np.abs(places - anchor).argmin()]
                if top > best_length or abs(place - anchor) < abs(best - anchor):
                    best, best_length = int(place), int(top)
        if best is None:
            return None
        return max(0, best - ANALOG_BEFORE)

    def elsewhere(self, start, end, length, random):
        """Where a passage from another place starts: of some places before
        and after the window, usually the one sharing most words with the
        window's middle half. The last quarter is left out of the choice, as
        the editor chooses without knowing what will be written."""
        low = max(0, start - FAR)
        high = min(self.count - ELSEWHERE - 1, end + REFERENCE + FAR)
        after = min(end + REFERENCE, high)
        places = np.concatenate([
            random.integers(low, max(low + 1, start - ELSEWHERE), CANDIDATES // 2),
            random.integers(after, max(after + 1, high), CANDIDATES // 2),
        ])
        if self.analog_share and random.random() < self.analog_share:
            found = self.analog(start, end, length, random)
            if found is not None:
                return found
        if random.random() < 0.2:
            return places[0]
        middle = self.token_ids(np.arange(start + length // 4, start + 3 * length // 4))
        wanted = np.unique(middle[self.wordy[middle]])
        found = self.token_ids(places[:, None] + np.arange(ELSEWHERE)[None, :])
        shared = (np.isin(found, wanted) & self.wordy[found]).sum(1)
        return places[shared.argmax()]

    def layout(self, random):
        """How many tokens a batch's windows get from elsewhere and from
        below. A batch shares one layout, so that no position is empty."""
        elsewhere = ELSEWHERE if random.random() < self.elsewhere_share else 0
        below = 0
        if random.random() < self.below_share:
            # Mostly a full passage; sometimes the file ends soon.
            below = BELOW if random.random() < 0.8 else int(random.integers(8, BELOW))
        return elsewhere, below

    def rows(self, starts, length, random, layout=None):
        """The windows starting at `starts` with their passages before them,
        as arrays still on the CPU."""
        count = len(starts)
        elsewhere, below = layout or self.layout(random)
        reference = np.zeros((count, elsewhere + below), dtype=np.int64)
        origin = np.zeros((count, elsewhere + below + length), dtype=np.uint8)
        for row, start in enumerate(starts):
            end = start + length + 1
            if elsewhere:
                reference[row, :elsewhere] = self.elsewhere(start, end, length, random) + np.arange(elsewhere)
            if below:
                # Mostly what follows at once.
                skip = 0 if random.random() < 0.7 else int(random.integers(1, 64))
                reference[row, elsewhere:] = end + skip + np.arange(below)
                # The end of the window is being typed in above that text,
                # as in the editor: there the text below follows the caret
                # at every step, and what was typed since reading began is
                # marked. A few tokens are typed, or any number.
                typed = int(random.integers(0, 97)) if random.random() < 0.5 else int(random.integers(0, length + 1))
                origin[row, elsewhere + below :] = 1
                origin[row, elsewhere + below + length - typed :] = 2
        index = np.concatenate([reference, starts[:, None] + np.arange(length + 1)[None, :]], 1)
        return {
            "layout": (elsewhere, below),
            "origin": origin,
            "tokens": self.token_ids(index),
            "language": self.language[starts + length].astype(np.int64),
        }

    def batch(self, rows, random=None, language_dropout=0.0):
        """Move windows to the device and finish them."""
        device = self.device
        tensor = lambda key: torch.from_numpy(rows[key]).to(device, non_blocking=True)
        tokens, language, origin = tensor("tokens"), tensor("language"), tensor("origin").long()
        if language_dropout:
            language = language.masked_fill(torch.rand(len(language), device=device) < language_dropout, 0)
        reference = sum(rows["layout"])
        return {
            "layout": rows["layout"],
            "tokens": tokens[:, :-1],
            "origin": origin,
            "language": language,
            "next": tokens[:, reference + 1 :],
        }


class Feed:
    """Cuts training windows on other threads while the device computes."""

    def __init__(self, data, size, length, seed, workers=4):
        self.ready = queue.Queue(maxsize=4 * workers)
        self.stopped = False

        def work(seed):
            random = np.random.default_rng(seed)
            while not self.stopped:
                starts = random.integers(MARGIN, data.count - length - MARGIN - 2, size)
                rows = data.rows(starts, length, random)
                while not self.stopped:
                    try:
                        self.ready.put(rows, timeout=0.5)
                        break
                    except queue.Full:
                        pass

        self.threads = [threading.Thread(target=work, args=(seed + index,), daemon=True) for index in range(workers)]
        for thread in self.threads:
            thread.start()

    def next(self):
        return self.ready.get()

    def stop(self):
        self.stopped = True


@torch.no_grad()
def evaluate(model, data, count, keys, sizes, weights=None, size=8, layout=(ELSEWHERE, BELOW)):
    """Top-1 accuracy, loss per token and per byte over evenly spaced
    windows; also the accuracy with two bytes of the answer typed."""
    length = model.length
    random = np.random.default_rng(7)
    starts = np.linspace(MARGIN, data.count - length - MARGIN - 2, count).astype(np.int64)
    device = data.device
    totals = torch.zeros(6, dtype=torch.float64, device=device)
    # Score the last three quarters of each window: in the editor the model
    # nearly always has at least that much context.
    scored = torch.arange(length // 4, length, device=device)
    for at in range(0, count, size):
        batch = data.batch(data.rows(starts[at : at + size], length, random, layout))
        positions = scored[None, ::4].expand(len(batch["language"]), -1)
        with torch.autocast(device, dtype=torch.bfloat16, enabled=device == "cuda"):
            result = model.loss(batch, weights, typed=(positions, torch.full_like(positions, 2), keys))
        part = slice(length // 4, None)
        correct, losses = result["correct"][:, part].double(), result["loss"][:, part].double()
        usable = result["typed_usable"]
        totals += torch.stack([
            correct.sum(), torch.tensor(correct.numel(), device=device), losses.sum(),
            sizes[batch["next"][:, part]].double().sum(),
            result["typed_correct"].double().sum(), usable.sum(),
        ])
    t = totals.tolist()
    return {"top1": t[0] / t[1], "loss": t[2] / t[1], "bits": t[2] / t[3] / math.log(2), "typed": t[4] / max(t[5], 1)}


def report(result):
    return (
        f"top-1 {result['top1']:.4f} loss {result['loss']:.4f} ({result['bits']:.3f} bits a byte; "
        f"with 2 typed {result['typed']:.4f})"
    )


class Cooler:
    """Rests between steps so that the graphics card stays at or below a
    temperature: 65 degrees unless a file names another limit. The file is
    read again every few steps, so the limit can change while training.
    There is always a limit: a loud card disturbs the people near it."""

    DEFAULT = 65.0

    def __init__(self, path=None):
        self.path, self.rest, self.steps, self.limit, self.heat = path, 0.0, 0, self.DEFAULT, None

    def pause(self):
        """Rest after a step; the seconds rested."""
        self.steps += 1
        if self.steps % 5 == 0:
            self.limit = self.DEFAULT
            if self.path:
                try:
                    self.limit = min(float(open(self.path).read().split()[0]), 90.0)
                except (OSError, ValueError, IndexError):
                    pass
            try:
                self.heat = float(subprocess.run(
                    ["nvidia-smi", "--query-gpu=temperature.gpu", "--format=csv,noheader,nounits"],
                    capture_output=True, text=True, timeout=10,
                ).stdout.split()[0])
            except (OSError, ValueError, IndexError, subprocess.SubprocessError):
                self.heat = None
            if self.heat is None:
                # Not knowing the heat, go gently.
                self.rest = max(self.rest, 0.5)
            else:
                # Rests grow quickly while it is too warm, shrink while there is room.
                over = self.heat - self.limit + 1
                self.rest = min(max(self.rest + (0.15 if over > 0 else 0.02) * over, 0.0), 5.0)
                if self.heat >= self.limit + 3:
                    # Well over: cool down before going on.
                    time.sleep(10.0)
        if self.rest > 0:
            time.sleep(self.rest)
        return self.rest


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("directory")
    parser.add_argument("--out", required=True)
    parser.add_argument("--minutes", type=float, default=420.0, help="time for general training")
    parser.add_argument("--tune", type=float, default=60.0, help="time for tuning to completion afterwards")
    parser.add_argument("--batch", type=int, default=16)
    parser.add_argument("--rate", type=float, default=1.0e-3)
    parser.add_argument("--width", type=int, default=384)
    parser.add_argument("--layers", type=int, default=10)
    parser.add_argument("--heads", type=int, default=8)
    parser.add_argument("--length", type=int, default=1024)
    parser.add_argument("--checkpoint", help="file to save progress to, and to resume from when it exists")
    parser.add_argument(
        "--again", action="store_true",
        help="resume the weights but start the clock anew; general training resumes from the weights saved before tuning",
    )
    parser.add_argument(
        "--cool",
        help="file holding the highest temperature of the graphics card in degrees (65 without it); rests do not count as training time",
    )
    parser.add_argument("--vectors", help="write test vectors for the editor's implementation")
    parser.add_argument(
        "--analog", type=float, default=0.0,
        help="share of passages from elsewhere taken from the analogous place (as the editor's pattern predictor finds it)",
    )
    arguments = parser.parse_args()

    device = "cuda" if torch.cuda.is_available() else "cpu"
    torch.manual_seed(0)
    torch.backends.cuda.matmul.allow_tf32 = True
    directory = arguments.directory
    merges_blob = open(f"{directory}/subwords.bin", "rb").read()
    spellings = read_subwords(merges_blob)
    vocabulary = len(spellings)
    to_device = lambda array: torch.from_numpy(array).to(device)
    wordy = np.array([len(text.strip()) >= 4 and text.strip().isalpha() for text in spellings])
    keys = torch.stack([to_device(prefix_keys(spellings, count)) for count in range(5)])
    sizes = to_device(np.array([max(len(text), 1) for text in spellings], dtype=np.float32))
    spaced = to_device(np.array([text[:1] in (b" ", b"\n") for text in spellings]))
    # How much typing each answer saves: its length, within limits.
    saving = 0.5 + sizes.clamp(max=12) / 6
    streams = {name: np.memmap(f"{directory}/{name}.tok", dtype=TOKEN, mode="r") for name in ("train", "valid", "unseen")}
    data = {name: Data(tokens, wordy, device) for name, tokens in streams.items()}
    for part in data.values():
        part.analog_share = arguments.analog
    train = data["train"]
    print(
        f"{device}: {vocabulary} tokens, {train.count} training, {data['valid'].count} validation "
        f"and {data['unseen'].count} unseen-language tokens",
        flush=True,
    )

    model = Model(vocabulary, arguments.width, arguments.layers, arguments.heads, arguments.length).to(device)
    print(f"{sum(p.numel() for p in model.parameters())} weights", flush=True)
    optimizer = torch.optim.AdamW(model.parameters(), lr=arguments.rate, betas=(0.9, 0.98), weight_decay=0.01)
    length = arguments.length
    general, tuning = arguments.minutes * 60, arguments.tune * 60
    step, spent = 0, 0.0
    # Weights as general training left them, before tuning narrowed them to
    # completion. A longer run started `--again` continues from these.
    before_tuning = arguments.checkpoint and arguments.checkpoint + ".general"
    resume = arguments.checkpoint
    if arguments.again and general > 0 and before_tuning and os.path.exists(before_tuning):
        resume = before_tuning
    if resume and os.path.exists(resume):
        saved = torch.load(resume, map_location=device)
        model.load_state_dict(saved["model"])
        optimizer.load_state_dict(saved["optimizer"])
        step, spent = saved["step"], 0.0 if arguments.again else saved["spent"]
        print(f"resumed from {resume} at step {step}, {spent / 60:.1f} min", flush=True)
    first_step = step if arguments.again else 0
    saved_general = not before_tuning or spent >= general
    feed = Feed(train, arguments.batch, length, seed=1 + step)
    random = np.random.default_rng(1 + step)
    started = time.time() - spent
    running, running_typed, last_saved = 0.0, 0.0, time.time()
    scored = torch.arange(length // 4, length, device=device)
    cooler = Cooler(arguments.cool)
    while True:
        elapsed = time.time() - started
        if elapsed >= general + tuning:
            break
        tune = elapsed >= general
        if tune and not saved_general:
            torch.save({"model": model.state_dict(), "optimizer": optimizer.state_dict(), "step": step, "spent": elapsed}, before_tuning)
            saved_general = True
        if tune:
            share = (elapsed - general) / tuning
            rate = arguments.rate * (0.01 + 0.14 * 0.5 * (1 + math.cos(math.pi * share)))
        else:
            # Warmed up anew at the start of every run.
            rate = arguments.rate * min(1.0, (step - first_step + 1) / 1500) * (
                0.05 + 0.95 * 0.5 * (1 + math.cos(math.pi * elapsed / general))
            )
        for group in optimizer.param_groups:
            group["lr"] = rate
        batch = train.batch(feed.next(), random, language_dropout=0.25)
        with torch.autocast(device, dtype=torch.bfloat16, enabled=device == "cuda"):
            if tune:
                # Random scored positions, each asked with one to three bytes
                # of the word typed (a space or line end before it counts too).
                picks = torch.rand(arguments.batch, len(scored), device=device).argsort(1)[:, :96]
                positions = scored[picks]
                answer = batch["next"].gather(1, positions)
                lengths = torch.randint(1, 4, positions.shape, device=device) + spaced[answer].long()
                result = model.loss(batch, model.as_stored(), typed=(positions, lengths, keys))
            else:
                result = model.loss(batch)
        if tune:
            weight = saving[batch["next"]]
            weight[:, : length // 4] *= 0.25
            loss = (result["loss"] * weight).sum() / weight.sum()
            usable = result["typed_usable"]
            typed_loss = (result["typed_loss"] * usable).sum() / usable.sum().clamp(min=1)
            running_typed += float(typed_loss.detach())
            loss = loss + 0.5 * typed_loss
        else:
            loss = result["loss"].mean()
        optimizer.zero_grad(set_to_none=True)
        loss.backward()
        torch.nn.utils.clip_grad_norm_(model.parameters(), 1.0)
        optimizer.step()
        running += float(result["loss"].detach().mean())
        step += 1
        if cooler:
            started += cooler.pause()
        if step % 500 == 0:
            weights = model.as_stored() if tune else None
            valid = evaluate(model, data["valid"], 96, keys, sizes, weights)
            unseen = evaluate(model, data["unseen"], 48, keys, sizes, weights)
            print(
                f"step {step}  {elapsed / 60:.1f} min  {'tune' if tune else 'general'}  train loss {running / 500:.3f}"
                f"{f'  {cooler.heat:.0f} C (limit {cooler.limit:.0f}), resting {cooler.rest:.2f} s a step' if cooler.heat is not None else ''}"
                f"{f' typed {running_typed / 500:.3f}' if tune else ''}\n  valid  {report(valid)}\n  unseen {report(unseen)}",
                flush=True,
            )
            running, running_typed = 0.0, 0.0
        if arguments.checkpoint and time.time() - last_saved > 900:
            torch.save({"model": model.state_dict(), "optimizer": optimizer.state_dict(), "step": step, "spent": time.time() - started}, arguments.checkpoint + ".new")
            os.replace(arguments.checkpoint + ".new", arguments.checkpoint)
            last_saved = time.time()
    feed.stop()
    print(
        f"{step} steps, {step * arguments.batch * length / max(time.time() - started, 1):.0f} tokens a second, "
        f"{torch.cuda.max_memory_allocated() / 2**30 if device == 'cuda' else 0:.1f} GiB at most",
        flush=True,
    )
    if arguments.checkpoint:
        torch.save({"model": model.state_dict(), "optimizer": optimizer.state_dict(), "step": step, "spent": time.time() - started}, arguments.checkpoint)

    stored, restored = model.stored()
    for name in ("valid", "unseen"):
        full = evaluate(model, data[name], 1000, keys, sizes)
        compact = evaluate(model, data[name], 1000, keys, sizes, restored)
        print(f"final {name}\n  trained   {report(full)}\n  as stored {report(compact)}")
        for label, layout in (("neither passage", (0, 0)), ("only elsewhere", (ELSEWHERE, 0)), ("only below", (0, BELOW))):
            print(f"  {label}: {report(evaluate(model, data[name], 1000, keys, sizes, restored, layout=layout))}")

    with open(arguments.out, "wb") as out:
        out.write(MAGIC)
        out.write(merges_blob)
        out.write(struct.pack("<IIIIII", arguments.width, arguments.layers, arguments.heads, arguments.length, ELSEWHERE, BELOW))
        for name in model.order():
            scale, values = stored[name]
            if scale is not None:
                out.write(struct.pack("<f", scale))
            out.write(values.tobytes())
    print(f"wrote {arguments.out}")

    if arguments.vectors:
        # A few contexts with the stored model's own answers, for checking
        # that the editor's implementation computes the same function. A
        # context is the passage from elsewhere, the text below, then the
        # window, each token with its mark of how it came to stand there.
        valid = data["valid"]
        torch.backends.cuda.matmul.allow_tf32 = False  # Exact answers to compare with.
        cases = [(120, (ELSEWHERE, BELOW)), (120, (0, 40)), (120, (ELSEWHERE, 0)), (120, (0, 0)), (length, (ELSEWHERE, BELOW))]
        starts = np.linspace(MARGIN, valid.count - 2 * length - MARGIN - 2, len(cases)).astype(np.int64)
        with open(arguments.vectors, "wb") as out:
            out.write(struct.pack("<I", len(cases)))
            for (size, layout), start in zip(cases, starts):
                batch = valid.batch(valid.rows(np.array([start]), size, np.random.default_rng(5), layout))
                with torch.no_grad():
                    scores = model.forward(batch, restored)
                joint = scores[0, -1].double().softmax(-1)
                out.write(struct.pack("<IIII", layout[0], layout[1], size, int(batch["language"][0])))
                out.write(batch["tokens"][0].cpu().numpy().astype("<u2").tobytes())
                out.write(batch["origin"][0].cpu().numpy().astype(np.uint8).tobytes())
                out.write(joint.cpu().numpy().astype("<f4").tobytes())
        print(f"wrote {arguments.vectors}")


if __name__ == "__main__":
    main()
