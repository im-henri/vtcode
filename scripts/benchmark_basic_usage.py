"""Alternate identical optimized test workloads, keeping raw logs and digests."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import statistics
import subprocess


def microseconds(number, unit):
    return float(number) * {"ns": .001, "µs": 1, "ms": 1000, "s": 1e6}[unit]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--output", type=Path, default=Path("target/basic-usage-paired"))
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--frames", action="store_true", help="include the longer input plus full-frame regression workload")
    parser.add_argument("--affinity-mask", type=lambda text: int(text, 0), help="Windows CPU mask inherited by the benchmark children")
    args = parser.parse_args()
    if args.affinity_mask is not None:
        if os.name != "nt":
            parser.error("--affinity-mask requires Windows")
        import ctypes
        from ctypes import wintypes
        affinity = ctypes.WinDLL("kernel32", use_last_error=True).SetProcessAffinityMask
        affinity.argtypes = [wintypes.HANDLE, ctypes.c_size_t]
        affinity.restype = wintypes.BOOL
        if not affinity(wintypes.HANDLE(-1), args.affinity_mask):
            raise ctypes.WinError(ctypes.get_last_error())
    args.output.mkdir(parents=True, exist_ok=True)
    binaries = {"before": args.baseline.resolve(), "after": args.candidate.resolve()}
    tests = ["benchmark_basic_paste_operations", "benchmark_routine_content_switches", "benchmark_incremental_content_analysis", "benchmark_basic_file_and_edit_work"]
    if args.frames:
        tests.append("benchmark_unwrapped_input_frames")
    runs = []
    for round_id in range(args.rounds):
        for side in (["before", "after"] if round_id % 2 == 0 else ["after", "before"]):
            run = dict(round=round_id + 1, side=side, paste=[], tabAnalysis=[], contentAnalysis=[], basicIO=[], frames=[])
            prefix = args.output / f"{round_id + 1}-{side}"
            env = os.environ.copy()
            report = prefix.with_name(prefix.name + "-paste.json")
            env["VTCODE_BASIC_PASTE_REPORT"] = str(report.resolve())
            for test in tests:
                log = prefix.with_name(prefix.name + f"-{test}.log")
                with log.open("w", encoding="utf-8") as stream:
                    subprocess.run([str(binaries[side]), test, "--ignored", "--nocapture", "--test-threads=1"], check=True, env=env, stdout=stream, stderr=subprocess.STDOUT)
                text = log.read_text(encoding="utf-8")
                if test == "benchmark_routine_content_switches":
                    run["tabAnalysis"] = [dict(lines=int(lines), calls=int(calls), medianUsPerCall=int(ns) / int(calls) / 1000) for lines, calls, ns in re.findall(r"lines=(\d+), switches=(\d+), median_ns=(\d+)", text)]
                elif test == "benchmark_incremental_content_analysis":
                    run["contentAnalysis"] = [dict(lines=int(line), referenceUs=microseconds(a, au), coldUs=microseconds(b, bu), editUs=microseconds(c, cu)) for line, a, au, b, bu, c, cu in re.findall(r"content analysis (\d+) lines: full reference=([\d.]+)(ns|µs|ms|s), cold=([\d.]+)(ns|µs|ms|s), local edit=([\d.]+)(ns|µs|ms|s)", text)]
                elif test == "benchmark_basic_file_and_edit_work":
                    run["basicIO"] = [dict(case=case, medianUs=float(us)) for case, us in re.findall(r"BASIC (.*): ([\d.]+) us", text)]
                elif test == "benchmark_unwrapped_input_frames":
                    run["frames"] = [dict(case=case, animations=animations == "true", medianUs=microseconds(value, unit)) for case, animations, value, unit in re.findall(r"unwrapped (\w+) animations=(true|false): .*new=([\d.]+)(ns|µs|ms|s)", text)]
            run["paste"] = json.loads(report.read_text(encoding="utf-8"))
            runs.append(run)
            print(f'Completed {side}, round {round_id + 1}', flush=True)
        before, after = [next(run for run in runs if run["round"] == round_id + 1 and run["side"] == side) for side in ["before", "after"]]
        for a, b in zip(before["paste"], after["paste"]):
            for key in ["case", "inputBytes", "outputSha256", "cursor", "undo"]:
                assert a[key] == b[key], (key, a, b)
        assert len(before["paste"]) == len(after["paste"]) == 16
        for run in [before, after]:
            assert len(run["tabAnalysis"]) == 2 and len(run["contentAnalysis"]) == 2 and len(run["basicIO"]) == 8
            assert not args.frames or len(run["frames"]) == 8
    summary = {}
    for kind, key_fields, metrics in [
        ("paste", ["case", "inputBytes"], ["medianUs"]),
        ("tabAnalysis", ["lines", "calls"], ["medianUsPerCall"]),
        ("contentAnalysis", ["lines"], ["referenceUs", "coldUs", "editUs"]),
        ("basicIO", ["case"], ["medianUs"]),
        ("frames", ["case", "animations"], ["medianUs"]),
    ]:
        grouped = {}
        for run in runs:
            for row in run[kind]:
                key = tuple(row[field] for field in key_fields)
                group = grouped.setdefault(key, {field: row[field] for field in key_fields})
                group.setdefault(run["side"], {metric: [] for metric in metrics})
                for metric in metrics:
                    group[run["side"]][metric].append(row[metric])
        summary[kind] = [{**group, "medians":{side:{metric:statistics.median(values) for metric,values in group[side].items()} for side in binaries}} for group in grouped.values()]
    output = dict(binaries={side:dict(name=path.name, sha256=hashlib.sha256(path.read_bytes()).hexdigest()) for side,path in binaries.items()}, rounds=args.rounds, affinityMask=args.affinity_mask, frames=args.frames, summary=summary, runs=runs)
    path = args.output / "comparison.json"
    path.write_text(json.dumps(output, indent=2) + "\n", encoding="utf-8")
    print(path)


if __name__ == "__main__":
    main()
