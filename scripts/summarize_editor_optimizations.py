"""Summarize alternating editor benchmarks without discarding the raw samples."""
import argparse
import json
from pathlib import Path
import re
from statistics import median


def duration_ns(value):
    match = re.fullmatch(r"([\d.]+)(ns|[µμu]s|ms|s)", value)
    if match is None:
        raise ValueError(f"unrecognized duration: {value}")
    scale = {"ns": 1, "µs": 1000, "μs": 1000, "us": 1000,
             "ms": 1_000_000, "s": 1_000_000_000}[match[2]]
    return round(float(match[1]) * scale)


def comparison(before, after):
    old, new = median(before), median(after)
    return {"beforeNs": old, "afterNs": new, "ratioAfterToBefore": new / old,
            "beforeRunMediansNs": before, "afterRunMediansNs": after}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--rich-followup", type=Path)
    parser.add_argument("--rich-affinity-mask")
    args = parser.parse_args()
    report = json.loads((args.input / "comparison.json").read_text(encoding="utf-8-sig"))
    runs = report.pop("runs")
    summary = {"provenance": report, "symbols": [], "mentions": [],
               "inputFrames": [], "richFrames": []}
    sides = {side: sorted((r for r in runs if r["side"] == side), key=lambda r: r["round"])
             for side in ("before", "after")}
    for side, values in sides.items():
        if len(values) != report["rounds"]:
            raise ValueError(f"missing {side} runs")
    for case in sides["before"][0]["symbols"]["cases"]:
        name = case["case"]
        values = {side: [next(c for c in r["symbols"]["cases"] if c["case"] == name)
                         for r in group] for side, group in sides.items()}
        for group in values.values():
            for row in group:
                if any(row[k] != case[k] for k in ("files", "bytes", "symbols", "digest")):
                    raise ValueError(f"changed source-symbol output: {name}")
        item = {k: case[k] for k in ("case", "files", "bytes", "symbols", "digest", "samples")}
        for field in ("dispatchMedianNs", "readyMedianNs", "longestApplyMedianNs",
                      "outlineDispatchNs", "outlineReadyNs"):
            before, after = ([r[field] for r in values[side]] for side in ("before", "after"))
            # Synchronous baseline has no result-application turn to time.
            item[field] = comparison(before, after) if median(before) else {
                "beforeNs": 0, "afterNs": median(after),
                "beforeRunMediansNs": before, "afterRunMediansNs": after}
        summary["symbols"].append(item)
    for case in sides["before"][0]["mentions"]:
        item = {k: case[k] for k in ("case", "bytes", "cursor", "samples")}
        values = {side: [next(c["medianNs"] for c in r["mentions"]
                              if (c["case"], c["bytes"]) == (case["case"], case["bytes"]))
                         for r in group] for side, group in sides.items()}
        item.update(comparison(values["before"], values["after"]))
        summary["mentions"].append(item)
    if report["framesIncluded"]:
        frames, rich = {}, {}
        for side, group in sides.items():
            for run in group:
                prefix = args.input / f"{side}-{run['round']}"
                text = Path(f"{prefix}-frames.log").read_text(encoding="utf-8-sig")
                matches = re.findall(r"unwrapped (\w+) animations=(true|false): old=\S+, new=(\S+),", text)
                if len(matches) != 8:
                    raise ValueError(f"missing input frames: {prefix}")
                for case, animations, duration in matches:
                    key = (case, animations == "true")
                    frames.setdefault(key, {"before": [], "after": []})[side].append(duration_ns(duration))
                text = Path(f"{prefix}-rich.log").read_text(encoding="utf-8-sig")
                matches = re.findall(r"rich render case=(\w+) median=(\S+) cells=([0-9a-f]+)", text)
                if len(matches) != 5:
                    raise ValueError(f"missing rich frames: {prefix}")
                for case, duration, digest in matches:
                    entry = rich.setdefault(case, {"digest": digest, "before": [], "after": []})
                    if digest != entry["digest"]:
                        raise ValueError(f"changed rendered cells: {case}")
                    entry[side].append(duration_ns(duration))
        for (case, animations), values in frames.items():
            summary["inputFrames"].append({"case": case, "animations": animations,
                                            **comparison(values["before"], values["after"])})
        for case, values in rich.items():
            summary["richFrames"].append({"case": case, "digest": values["digest"],
                                           **comparison(values["before"], values["after"])})
    if args.rich_followup:
        rich = {}
        for path in sorted(args.rich_followup.glob("*-rich.log")):
            side = path.name.split("-")[0]
            if side not in sides:
                raise ValueError(f"unknown benchmark side: {path}")
            text = path.read_text(encoding="utf-8-sig")
            matches = re.findall(r"rich render case=(\w+) median=(\S+) cells=([0-9a-f]+)", text)
            if len(matches) != 5:
                raise ValueError(f"missing rich frames: {path}")
            for case, duration, digest in matches:
                entry = rich.setdefault(case, {"digest": digest, "before": [], "after": []})
                if digest != entry["digest"]:
                    raise ValueError(f"changed rendered cells: {case}")
                entry[side].append(duration_ns(duration))
        counts = {len(entry[side]) for entry in rich.values() for side in sides}
        if len(counts) != 1 or not rich:
            raise ValueError("incomplete rich-render follow-up pairs")
        summary["richFollowup"] = {"affinityMask": args.rich_affinity_mask, "rounds": counts.pop(),
                                   "cases": [{"case": case, "digest": values["digest"],
                                              **comparison(values["before"], values["after"])}
                                             for case, values in rich.items()]}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    checked = "symbol and rendered-cell digests" if report["framesIncluded"] else "symbol digests"
    print(f"Wrote {args.output}: {report['rounds']} alternating rounds; {checked} match.")


if __name__ == "__main__":
    main()
