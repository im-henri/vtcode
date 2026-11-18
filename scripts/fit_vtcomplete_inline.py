#!/usr/bin/env python3
"""Fit the VTComplete inline-prediction calibration model.

The predictor votes on each next token; this model estimates the probability
that a voted token is right from the evidence behind it. It is a logistic
regression over the one-hot features produced by `Step::features` in
`src/vtcomplete_inline.rs`.

Training rows come from the replay benchmark:

    cargo build --profile fastrelease --example vtcomplete_inline_bench
    vtcomplete_inline_bench --first 0.02 --next 0.02 --block 0.02 --floor 0 \
        --dump rows.csv PATH...

Each row is `label,fold,feature,...`. Fold 0 files train the model and fold 1
files of the same corpora report held-out calibration. `--write` replaces the
weight table in the Rust source.
"""
import argparse
import re
import sys

import numpy as np

FEATURES = 158
ABSENT = 94  # Shared slot for flags that are off; its weight stays zero.


def load(paths):
    labels, folds, rows = [], [], []
    for path in paths:
        with open(path, encoding="utf-8") as source:
            for line in source:
                parts = line.split(",")
                labels.append(int(parts[0]))
                folds.append(int(parts[1]))
                rows.append([int(value) for value in parts[2:]])
    return np.array(labels, dtype=np.float64), np.array(folds), np.array(rows)


def probability(weights, rows):
    return 1.0 / (1.0 + np.exp(-(weights[rows].sum(axis=1) + weights[FEATURES])))


def fit(labels, rows, penalty):
    """Weights minimizing the log loss plus a small penalty, to convergence.
    PyTorch (already needed to train the language model) does the search."""
    import torch

    rows_t = torch.from_numpy(rows).long()
    labels_t = torch.from_numpy(labels).double()
    weights = torch.zeros(FEATURES + 1, dtype=torch.double, requires_grad=True)
    free = torch.ones(FEATURES + 1, dtype=torch.double)
    free[ABSENT] = 0.0
    search = torch.optim.LBFGS([weights], max_iter=500, tolerance_grad=1e-9, tolerance_change=1e-12, line_search_fn="strong_wolfe")

    def loss():
        search.zero_grad()
        used = weights * free
        scores = used[rows_t].sum(1) + used[FEATURES]
        value = torch.nn.functional.binary_cross_entropy_with_logits(scores, labels_t)
        value = value + 0.5 * penalty * (used[:FEATURES] ** 2).sum()
        value.backward()
        return value

    search.step(loss)
    return (weights * free).detach().numpy()


def report(name, weights, labels, rows):
    predicted = probability(weights, rows)
    loss = -np.mean(labels * np.log(predicted + 1e-9) + (1 - labels) * np.log(1 - predicted + 1e-9))
    print(f"{name}: rows {len(labels)}  log loss {loss:.4f}  base rate {labels.mean():.3f}")
    for low in np.arange(0.0, 1.0, 0.1):
        chosen = (predicted >= low) & (predicted < low + 0.1)
        if chosen.sum() >= 50:
            print(
                f"  predicted {low:.1f}-{low + 0.1:.1f}: n {int(chosen.sum()):7d}"
                f"  actual {labels[chosen].mean():.3f}"
            )


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("rows", nargs="+")
    parser.add_argument("--holdout", nargs="*", default=[])
    parser.add_argument("--penalty", type=float, default=1e-4)
    parser.add_argument("--write", metavar="RUST_SOURCE")
    parser.add_argument(
        "--table", default="WEIGHTS",
        help="WEIGHTS for rows dumped with the language model, WEIGHTS_ALONE for rows dumped with --no-model",
    )
    arguments = parser.parse_args()

    labels, folds, rows = load(arguments.rows)
    train = folds == 0
    weights = fit(labels[train], rows[train], arguments.penalty)
    report("train", weights, labels[train], rows[train])
    if (~train).any():
        report("held-out files", weights, labels[~train], rows[~train])
    for path in arguments.holdout:
        held_labels, _, held_rows = load([path])
        report(f"held-out corpus {path}", weights, held_labels, held_rows)

    values = ", ".join(f"{value:.4f}" for value in weights)
    marker = "FITTED WEIGHTS" + arguments.table[len("WEIGHTS"):].replace("_", " ")
    table = (
        f"// BEGIN {marker}\n#[rustfmt::skip]\n"
        "#[allow(clippy::approx_constant)] // Fitted numbers; any likeness to a constant is chance.\n"
        f"const {arguments.table}: [f32; FEATURES + 1] = [{values}];\n"
        f"// END {marker}"
    )
    if arguments.write:
        with open(arguments.write, encoding="utf-8") as source:
            text = source.read()
        updated, replaced = re.subn(
            rf"// BEGIN {marker}\n.*?// END {marker}(?=\n)", lambda _: table, text, flags=re.S
        )
        if replaced != 1:
            sys.exit("weight table markers not found")
        with open(arguments.write, "w", encoding="utf-8", newline="\n") as target:
            target.write(updated)
        print(f"wrote {arguments.write}")
    else:
        print(table)


if __name__ == "__main__":
    main()
