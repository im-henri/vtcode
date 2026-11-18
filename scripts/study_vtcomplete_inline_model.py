#!/usr/bin/env python3
"""Study what the VTComplete inline model knows when it writes a token.

Reads a directory written by the replay benchmark with `VT_RECORD=1 ...
vtcomplete_inline_bench --record DIR PATH...` and reports:

  1. how well right and wrong tokens are told apart by the token's
     probability alone, by those numbers together, and by the model's final
     state (a linear probe and a small network), on held-out files;
  2. a logit lens: what each layer, read out through the output matrix,
     would have chosen, and how often an earlier layer had the right token
     where the last layer chose wrong;
  3. neurons of the last layers whose activity differs most between right
     and wrong tokens, with the tokens they push up.

Runs on the CPU; nothing here needs a GPU.
"""
import argparse
import pathlib
import sys

import numpy as np


def bf16(path, width):
    raw = np.fromfile(path, dtype=np.uint16)
    return (raw.astype(np.uint32) << 16).view(np.float32).reshape(-1, width)


def auroc(scores, labels):
    order = np.argsort(scores)
    ranks = np.empty(len(scores))
    ranks[order] = np.arange(1, len(scores) + 1)
    positive = labels.sum()
    negative = len(labels) - positive
    return (ranks[labels == 1].sum() - positive * (positive + 1) / 2) / max(1, positive * negative)


def log_loss(p, y):
    p = np.clip(p, 1e-6, 1 - 1e-6)
    return float(-np.mean(y * np.log(p) + (1 - y) * np.log(1 - p)))


def fit_logistic(x, y, penalty=1e-3, steps=300):
    import torch

    x_t = torch.from_numpy(x).float()
    y_t = torch.from_numpy(y).float()
    weights = torch.zeros(x.shape[1], requires_grad=True)
    bias = torch.zeros(1, requires_grad=True)
    search = torch.optim.LBFGS([weights, bias], max_iter=steps, line_search_fn="strong_wolfe")

    def loss():
        search.zero_grad()
        value = torch.nn.functional.binary_cross_entropy_with_logits(x_t @ weights + bias, y_t)
        value = value + penalty * (weights**2).sum()
        value.backward()
        return value

    search.step(loss)
    return weights.detach().numpy(), float(bias.detach())


def fit_network(x, y, hidden=64, epochs=30, seed=0):
    import torch

    torch.manual_seed(seed)
    x_t = torch.from_numpy(x).float()
    y_t = torch.from_numpy(y).float()
    net = torch.nn.Sequential(torch.nn.Linear(x.shape[1], hidden), torch.nn.ReLU(), torch.nn.Linear(hidden, 1))
    opt = torch.optim.AdamW(net.parameters(), lr=2e-3, weight_decay=1e-2)
    for _ in range(epochs):
        order = torch.randperm(len(x_t))
        for start in range(0, len(order), 512):
            batch = order[start:start + 512]
            opt.zero_grad()
            loss = torch.nn.functional.binary_cross_entropy_with_logits(net(x_t[batch]).squeeze(1), y_t[batch])
            loss.backward()
            opt.step()
    return net


def scalars(rows):
    p = rows["probability"]
    return np.stack([
        np.log(np.clip(p, 1e-6, 1)),
        np.log(np.clip(rows["second"], 1e-6, 1)),
        rows["entropy"],
        np.log(np.clip(rows["agreeing"], 1e-6, 1)),
        np.log(np.clip(rows["chance"], 1e-9, 1)),
        np.minimum(rows["typed"], 8) / 8,
        np.minimum(rows["step"], 20) / 20,
        np.minimum(rows["bytes"], 12) / 12,
    ], axis=1).astype(np.float32)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("dirs", nargs="+", type=pathlib.Path)
    parser.add_argument("--width", type=int, default=384)
    parser.add_argument("--layers", type=int, default=10)
    parser.add_argument("--neurons", action="store_true", help="also rank neurons (slow)")
    args = parser.parse_args()
    width = args.width
    # Token spellings include any character; a console may not.
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")

    import csv

    columns = None
    rows, states, traced, trace_rows = [], [], [], []
    for offset, directory in enumerate(args.dirs):
        with open(directory / "steps.csv", newline="") as source:
            reader = csv.DictReader(source)
            part = list(reader)
        part_states = bf16(directory / "states.bin", width)
        assert len(part) == len(part_states), (directory, len(part), len(part_states))
        per = width * args.layers + 3 * 4 * width
        traces = bf16(directory / "traces.bin", per) if (directory / "traces.bin").stat().st_size else np.zeros((0, per), np.float32)
        for index, row in enumerate(part):
            row["source"] = offset
            if int(row["traced"]) >= 0:
                traced.append(len(rows) + index)
                trace_rows.append(traces[int(row["traced"])])
        rows.extend(part)
        states.append(part_states)
    states = np.concatenate(states)
    table = {key: np.array([float(row[key]) for row in rows]) for key in
             ["right", "spelling", "probability", "second", "entropy", "agreeing", "chance", "typed", "step", "bytes", "file", "source", "token"]}
    keep = table["spelling"] == 0
    # Files of odd index are held out.
    held = ((table["file"] + table["source"]) % 2 == 1)
    y = table["right"].astype(np.float32)
    print(f"steps {len(y)}  judged {keep.sum()}  right {y[keep].mean():.3f}")

    train, test = keep & ~held, keep & held
    s = scalars(table)
    print("\n1. Telling right from wrong (held-out files)")
    print(f"   probability alone      AUROC {auroc(table['probability'][test], y[test]):.4f}")
    w, b = fit_logistic(s[train], y[train])
    p = 1 / (1 + np.exp(-(s[test] @ w + b)))
    print(f"   all numbers (logistic) AUROC {auroc(p, y[test]):.4f}  log loss {log_loss(p, y[test]):.4f}")
    mean, spread = states[train].mean(0), states[train].std(0) + 1e-4
    z = ((states - mean) / spread).astype(np.float32)
    x = np.concatenate([s, z], axis=1)
    w, b = fit_logistic(x[train], y[train], penalty=3e-3)
    p = 1 / (1 + np.exp(-(x[test] @ w + b)))
    print(f"   + final state (linear) AUROC {auroc(p, y[test]):.4f}  log loss {log_loss(p, y[test]):.4f}")
    import torch

    net = fit_network(x[train], y[train])
    with torch.no_grad():
        p = torch.sigmoid(net(torch.from_numpy(x[test])).squeeze(1)).numpy()
    print(f"   + final state (MLP 64) AUROC {auroc(p, y[test]):.4f}  log loss {log_loss(p, y[test]):.4f}")
    direction = w[s.shape[1]:]
    top = np.argsort(-np.abs(direction))[:12]
    print("   state cells weighing most on 'wrong' (negative) or 'right':",
          ", ".join(f"{cell}:{direction[cell]:+.2f}" for cell in top))

    if not traced:
        return
    directory = args.dirs[0]
    weights = np.fromfile(directory / "weights.bin", dtype=np.float32)
    vocab_size = sum(1 for _ in open(directory / "vocab.txt"))
    at = 0

    def take(count):
        nonlocal at
        part = weights[at:at + count]
        at += count
        return part

    token = take(vocab_size * width).reshape(vocab_size, width)
    gain, bias, output_bias = take(width), take(width), take(vocab_size)
    blocks = []
    for _ in range(3):
        expand = take(4 * width * width).reshape(4 * width, width)
        expand_bias = take(4 * width)
        reduce = take(width * 4 * width).reshape(width, 4 * width)
        blocks.append((expand, expand_bias, reduce))
    vocab = [bytes.fromhex(line.strip()) for line in open(directory / "vocab.txt")]

    traced = np.array(traced)
    trace_rows = np.stack(trace_rows)
    residual = trace_rows[:, :width * args.layers].reshape(len(traced), args.layers, width)
    neurons = trace_rows[:, width * args.layers:].reshape(len(traced), 3, 4 * width)
    chosen = table["token"][traced].astype(int)
    right = y[traced]
    judged = table["spelling"][traced] == 0

    # Each case's truth, to find the right token where the model chose wrong.
    cases = {}
    for directory_index, directory in enumerate(args.dirs):
        with open(directory / "cases.tsv") as source:
            next(source)
            for line in source:
                case, typed, written, truth = line.rstrip("\n").split("\t")
                cases[(directory_index, int(case))] = (bytes.fromhex(typed), bytes.fromhex(written), bytes.fromhex(truth))
    by_length = sorted(range(vocab_size), key=lambda t: -len(vocab[t]))

    def lens(layer_state):
        mu = layer_state.mean(-1, keepdims=True)
        sd = layer_state.std(-1, keepdims=True) + 1e-5
        normal = (layer_state - mu) / sd * gain + bias
        return normal @ token.T + output_bias

    print("\n2. Logit lens (traced steps)")
    wrong_rows = [i for i in range(len(traced)) if judged[i] and right[i] == 0]
    truth_token = {}
    for i in wrong_rows:
        row = rows[traced[i]]
        typed, written, truth = cases[(int(row["source"]), int(row["case"]))]
        step = int(row["step"])
        # Bytes written before this step, past the typed piece.
        ends = []
        # Reconstruct from step byte counts of this case.
        start = 0
        for other in range(traced[i] - step, traced[i]):
            start += int(rows[other]["bytes"])
        before = written[:start]
        done = max(0, len(before) - len(typed))
        remainder = truth[done:]
        pending = typed[len(before):] if len(before) < len(typed) else b""
        logits = lens(residual[i, -1])
        best = None
        for candidate in np.argsort(-logits)[:400]:
            spelling = vocab[candidate]
            if not spelling:
                continue
            if pending:
                if not (spelling[:len(pending)].lower() == pending[:len(spelling)].lower()):
                    continue
                tail = spelling[len(pending):]
            else:
                tail = spelling
            if tail and remainder.startswith(tail) or (not tail and len(spelling) <= len(pending)):
                best = candidate
                break
        if best is not None:
            truth_token[i] = best
    print(f"   wrong steps traced {len(wrong_rows)}, true token found among the top 400 for {len(truth_token)}")
    ranks = np.zeros((args.layers, 2))
    rescued = np.zeros(args.layers)
    final_rank = []
    for i, true in truth_token.items():
        for layer in range(args.layers):
            logits = lens(residual[i, layer])
            rank_true = int((logits > logits[true]).sum())
            ranks[layer, 0] += rank_true == 0
            ranks[layer, 1] += rank_true
            if layer == args.layers - 1:
                final_rank.append(rank_true)
            rescued[layer] += logits[true] > logits[chosen[i]]
    count = max(1, len(truth_token))
    for layer in range(args.layers):
        print(f"   layer {layer + 1:2d}: prefers the true token over the chosen one in {100 * rescued[layer] / count:5.1f}%  true token top-1 {100 * ranks[layer, 0] / count:5.1f}%")
    if final_rank:
        final_rank = np.array(final_rank)
        print(f"   final-layer rank of the true token when wrong: 2nd {100*np.mean(final_rank==1):.1f}%, top-5 {100*np.mean(final_rank<5):.1f}%, top-20 {100*np.mean(final_rank<20):.1f}%")

    if not args.neurons:
        return
    print("\n3. Neurons of the last 3 layers that differ most between right and wrong steps")
    ok = judged
    for layer in range(3):
        active = neurons[ok, layer]
        label = right[ok]
        a, b = active[label == 1], active[label == 0]
        difference = (b.mean(0) - a.mean(0)) / (active.std(0) + 1e-4)
        firing = (active > 0).mean(0)
        order = np.argsort(-np.abs(difference))[:8]
        expand, expand_bias, reduce = blocks[layer]
        print(f"   layer {args.layers - 2 + layer}:")
        for neuron in order:
            # What the neuron writes, read out through the output matrix.
            push = reduce[:, neuron] @ token.T
            ups = [vocab[t].decode("utf-8", "replace") for t in np.argsort(-push)[:6]]
            downs = [vocab[t].decode("utf-8", "replace") for t in np.argsort(push)[:4]]
            print(f"     n{neuron:4d} {'more' if difference[neuron] > 0 else 'less'} active when wrong ({difference[neuron]:+.2f} sd, fires {100*firing[neuron]:.0f}%)  up {ups}  down {downs}")


if __name__ == "__main__":
    main()
