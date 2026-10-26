"""Compare recorded outputs from two Git builds; no old predictor is retained.

python scripts/compare_completion_replays.py baseline.json current.json --output comparison.json
Supports the native-source, chronological editing, and source-renaming replays.
"""

import argparse
import json
from collections import defaultdict
from pathlib import Path


def identity(row):
    if "file" in row:
        return (row["file"], row["offset"])
    if "session" in row:
        return (row["session"], row["probe"], row["group"])
    return (row["source"], row.get("byte", row.get("probe")), row["group"])


def outcome(row):
    expected = row["expected"]
    if "rank" in row:
        rank, recall = row["rank"], row["recalled"]
        unwanted = False
    else:
        words = [word.removesuffix("()") for word in row["current"]]
        rank = words.index(expected) if expected in words else None
        recall = row["currentCandidateRecalled"]
        unwanted = expected is None and bool(words)
    return {
        "cases": int(expected is not None),
        "top1": int(expected is not None and rank == 0),
        "top5": int(expected is not None and rank is not None and rank < 5),
        "candidateRecall": int(expected is not None and recall),
        "negativeCases": int(expected is None),
        "unwanted": int(unwanted),
    }


def compare(baseline, current):
    for field in ("corpusSha256", "indexScope", "seedFilesPerSession", "warmIndex"):
        if field in baseline or field in current:
            assert baseline.get(field) == current.get(field), f"Different {field}"
    def events(report):
        seen = defaultdict(int)
        result = {}
        for row in report["rows"]:
            key = identity(row)
            # Designed sessions deliberately repeat some named probes. Keep
            # their chronological occurrence instead of silently overwriting.
            result[(key, seen[key])] = row
            seen[key] += 1
        return result

    before, after = events(baseline), events(current)
    assert before.keys() == after.keys(), "Different replay events"
    groups = defaultdict(lambda: {"baseline": defaultdict(int), "current": defaultdict(int)})
    changed = []
    for key, row in before.items():
        next_row = after[key]
        assert row["expected"] == next_row["expected"], f"Different answer at {key}"
        assert row.get("prefix") == next_row.get("prefix"), f"Different prefix at {key}"
        a_state, b_state = row.get("indexPublication"), next_row.get("indexPublication")
        if a_state or b_state:
            assert a_state["visibleSha256"] == b_state["visibleSha256"], f"Different visible text at {key}"
        a, b = outcome(row), outcome(next_row)
        for group in {"all", row.get("group", "native")}:
            for engine, values in (("baseline", a), ("current", b)):
                for metric, value in values.items():
                    groups[group][engine][metric] += value
        if a["top1"] != b["top1"] or a["candidateRecall"] != b["candidateRecall"]:
            changed.append({"event": key, "expected": row["expected"], "baseline": a, "current": b})
    for totals in groups.values():
        for values in totals.values():
            values["topChoiceErrors"] = values["cases"] - values["top1"]
    if "negativeCases" in baseline:
        # Native replays summarize impossible-prefix probes outside the row list.
        for group in ("all", "native"):
            for engine, report in (("baseline", baseline), ("current", current)):
                groups[group][engine]["negativeCases"] = report["negativeCases"]
                groups[group][engine]["unwanted"] = report["unwanted"]
    return {"events": len(before), "groups": dict(groups), "changedEvents": changed}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("current", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    report = compare(*(json.loads(path.read_text(encoding="utf-8-sig")) for path in (args.baseline, args.current)))
    if args.output:
        args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report["groups"], indent=2))


if __name__ == "__main__":
    main()
