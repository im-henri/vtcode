# VTComplete and GPTComplete

VTCode ships two completion engines that run entirely on your CPU: VTComplete for word suggestions and GPTComplete for inline, multi-token predictions from a built-in language model.

## VTComplete

VTComplete provides word suggestions from the active editor buffer, including
unsaved edits, along with language keywords and snippets. It scans the buffer
when suggestions are requested. **Tab** inserts a suggestion, **↑/↓** selects
another, **Esc** dismisses the popup, and **Ctrl+Space** requests suggestions.

Under **Settings → Autocomplete**, use **VTComplete** for word suggestions and
**GPTComplete** for inline predictions. **General → Show Suggestions While Typing**
controls automatic suggestions for both, as well as language-server completion.

## GPTComplete

GPTComplete is off by default. To enable it for your workspace, click
**GPTComplete Off** in the status bar and choose **Turn On GPTComplete**, or use
**Settings → Autocomplete → GPTComplete → Inline Predictions**.

While you type, GPTComplete predicts the next several tokens — often the
rest of the line, sometimes the following lines of a block — and shows them
dimmed after the caret. **Tab** inserts the prediction, **Ctrl+Right** inserts
its next word, **Esc** hides it, and typing along it keeps the remainder.
**Alt+I** (or **GPTComplete: Predict Here** in the Command Palette) shows the
best guess at the caret, including when **Show Suggestions While Typing** is off.
The word popup returns when no prediction is shown, after **Esc**, or with **Ctrl+Space**.

Click **GPTComplete** in the status bar to set **Eagerness**, snooze for **1, 5,
or 10 minutes**, choose **Snooze Until Resumed**, or turn GPTComplete off. The
workspace remembers whether GPTComplete is on, its eagerness, and untimed snoozes
across restarts. Snoozing or turning GPTComplete off also pauses **Alt+I**.

## How it works

Two predictors work together. Both run on one background CPU thread, need no
GPU and download nothing:

- **Patterns in the active file.** The predictor finds earlier places whose
  surrounding tokens match the caret, including places that differ only by
  consistently renamed identifiers (`self.left = left;` teaches
  `self.right = right;`, and `left_margin` → `right_margin` also turns
  `set_left_edge` into `set_right_edge`), and lets them vote on each next
  token.
- **A small language model.** A transformer of about 24 million weights
  (24 MB, one byte per weight) is built into the editor. It was trained on
  about 550 open-source projects in some forty languages and on prose. It
  reads and writes text in pieces smaller than words, about 16,000 of them
  learned from that code (`Layout`, `Render`, `ers`, ` self`, `.`, a line end
  with its indentation), down to single bytes. So it can write any text,
  including a name that stands nowhere yet, and it needs no rules for a
  language: comments, strings and documentation are text like everything
  else. Tokens cover more text than before, and the context is larger:

  - up to 1,024 tokens before the caret (several thousand characters);
  - the text just below the caret (128 tokens), so it knows what the code
    you are writing has to lead into;
  - a passage from elsewhere in the file (128 tokens), chosen for sharing
    the most uncommon words with the lines around the caret;
  - which tokens you typed since it started reading, as opposed to text
    that was already there, and, when known, the file's language.

  For a prediction the model writes ahead to the end of the line, starting
  with the letters of the word you are typing, and the engine weighs that
  line against what the patterns found, token by token. In training, some
  files have their commonest words replaced by made-up ones, so that it
  learns to pick up the keywords of a language of your own from the file in
  front of it, and seven languages are kept out of training altogether to
  measure that. After general training the model is tuned for completion:
  on the weights as they are stored, with long tokens counting for more,
  and to continue from one to three typed letters of a word.

  The model runs on the CPU with the editor's own code, on up to four
  low-priority threads. Results are kept per token and the window slides
  with the caret, so typing one more token computes one more position
  (about 2.5 ms on a desktop processor). Reading a whole window takes about
  0.2 s, so the editor does that ahead of time, when the caret comes to
  rest somewhere and after each answer. A prediction is allowed about
  160 ms; what did not fit is kept and continued by the next keystroke.

  Five things make more of the model without retraining it:

  - **It remembers what it has read.** Every position the model computes
    leaves a final state, and the token that came next is known. When the
    model chooses a token, the positions whose states are most like the
    current one (in the window, the passages, and up to 4,096 positions that
    have slid out of view) add what followed them to its choice: a
    nearest-neighbour memory of the file in the model's own terms, which
    copies the file's own idioms more reliably than a small model's
    attention does.
  - **The text below judges the line.** When the line being written starts
    with a doubtful choice, the few likeliest choices are each written out
    to the end of their line, and each line is scored by how likely the
    model then finds the real lines below the caret. The line that leads
    into the code below wins (Bayes' rule: how likely each choice is given
    the text on both sides). On doubtful lines in existing code this makes
    the whole line right about a quarter more often.
  - **It reads the analogous code.** The passage the model reads from
    elsewhere is the place the pattern predictor finds most like the
    caret's context (above the window, further below, or in another open
    file), starting just before it, so the model sees how that code went on
    and can adapt it. It is chosen again while the caret rests once you have
    typed on. Before, the passage was the place sharing the most names,
    which on replay helped no more than no passage at all.
  - **It writes on into blocks.** When the prediction opens a bracket or is
    inside one left open on the caret line, the model writes on past the
    line end with its own indentation until the block closes, so
    `if err != nil {` can be answered with its body and closing brace.
  - **Patterns are checked or not trusted.** When the time for a
    prediction runs out before the model could check a token the patterns
    propose, the prediction stops there unless the text points at that token
    firmly. Under load, unchecked patterns used to run on into confident
    wrong text.

  Studying the model on its own predictions (`scripts/study_vtcomplete_inline_model.py`)
  showed where the room was: its probabilities already tell right from wrong
  well (its final state, read by a trained probe, adds nothing), no layer
  knows better than the last, and no single neurons cause the errors. But
  when it is wrong, the right token is its second choice 38% of the time and
  among its first five 61% of the time: it needs more evidence to choose,
  which the memory and the text below supply.

  Measured by replaying twelve projects the model was never trained on, at
  the default eagerness: a suggestion appears at 77% of the positions tried
  and 75% of those shown are right (66% and 65% with the previous model), so
  a correct one appears at 57% of positions (43%). The whole rest of the
  line is right at 33% (16%), and 27% of the characters are saved (19%).
  With two characters of a word typed, the model alone picks the right
  token 88% of the time. In languages it has never seen (Julia, Nim, OCaml,
  Crystal, Gleam, Clojure), a correct suggestion appears at 47% of
  positions, against 19% from the patterns in the file alone. 38% of the
  comment lines tried get a correct continuation (3% from patterns alone).

  The memory, the judging by the text below, the checks under load and the
  blocks the model writes on into (all described above) were measured
  against the version without them on the same sample (six of the held-out
  projects, sixteen files each, the calibration refitted on training
  projects only): suggestions appear at 82.5% of positions instead of 77.8%,
  as often right as before (63.7%), so a correct one appears at 52.5% instead
  of 49.5%; the whole rest of the line is right at 30.1% instead of 27.8%,
  and 31.5% of the characters are saved instead of 27.9%. In the languages
  never trained on: correct at 44.9% instead of 41.6%, whole lines 19.1%
  instead of 17.3%, saved 16.8% instead of 14.7%. Reading the analogous
  code added, on three held-out projects, whole lines right 29.0% instead of
  27.7% and saved 28.6% instead of 27.6%, and in the unseen languages
  correct 45.3%, whole lines 19.9% and saved 17.4%, as often right as before.
  Training the model further on such passages (`--analog` in the trainer,
  35 minutes from the general weights) raised its next-token accuracy only
  from 73.6% to 73.8% and did not do better in the editor, so the model is
  unchanged.

## Completing names and series

A word you are typing is completed by the model and from the names in view.
Typing without capitals matches any capitalisation (`layoutr` finds
`LayoutRenderers`), and accepting restores the name's own spelling. When
what you type begins no existing name, the model writes a new one
(`struct LayoutT` gives `LayoutTexture`). A new name is a guess, so it is
shown only when the model is fairly sure, or when you ask with **Alt+I**,
which always shows its best guess.

Lines that count through a series are continued with its next step: after
`var item1 = a;` and `var item2 = b;` the prediction is `var item3 = c;`,
though `item3` exists nowhere yet. Numbers at the end of a name, numbers on
their own and letters count on; the steps must stand in neighbouring lines
just above the caret.

The model also scores continuations proposed by the pattern predictor, such
as a consistent rename or the next step in a series. GPTComplete weighs that
score alongside the evidence from the file.

## Confidence and eagerness

A small calibration model estimates how likely each token is to be right from
both kinds of evidence. A prediction stops at the first uncertain token, so
short answers are normal and no answer is common in code unlike anything
nearby. **Settings → Autocomplete → GPTComplete → Inline Prediction Eagerness**
and the status bar menu show each preset's minimum estimated confidence to
start an automatic suggestion. Lower thresholds allow more guesses. Each
preset also sets minimum confidence for later tokens, line breaks, and the
whole suggestion. **Balanced** is the default. **Alt+I** skips the starting
threshold to ask for the best available guess. Inside comments and strings only
the language model predicts, to the end of the line. Turn predictions off with **Settings →
Autocomplete → GPTComplete → Inline Predictions**.

The prediction is the main suggestion. While one is being worked out, the
word popup waits for it (up to a quarter of a second) instead of flashing up
only to be covered, unless the popup is already on screen; keys that would
act on a popup nobody has seen act as if it were not there. After answering,
the predictor works out ahead what it would suggest once you accept, so that
Tab after Tab shows the next suggestion at once.

## Tools for development

`examples/vtcomplete_inline_bench.rs` replays source files to measure accuracy
and latency and exports training data; `--try FILE` predicts at every `<|>`
mark in a file and prints the evidence behind each token. `scripts/fetch_vtcomplete_inline_corpus.py`
downloads the permissively licensed open-source projects the model is trained
on, `scripts/train_vtcomplete_inline_attention.py` trains it with PyTorch
(training only; the editor evaluates the stored weights with its own code),
and `scripts/fit_vtcomplete_inline.py` refits the calibration model.
`VT_RECORD=1 vtcomplete_inline_bench --record DIR PATH...` records what the
model wrote at each caret, token by token, with its states, each layer's
output and the last layers' neurons; `scripts/study_vtcomplete_inline_model.py DIR...`
reports how well right and wrong tokens can be told apart, a logit lens over
the layers, and the neurons that differ most between them.
`scripts/drive_vtcomplete_inline.py` starts the built editor in a
pseudo-terminal, types scenarios into it key by key, and checks the
prediction on screen and the text Tab inserts.

---

[← Back to README](../README.md)
