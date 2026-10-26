"""Run alternating baseline/candidate replays against the same frozen corpus.

Both test binaries must contain vtcomplete_generation_replay_tests.rs. The
baseline must otherwise use the original production source. Raw reports and logs
are preserved; the summary records fixture hashes and every run's measurements.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import statistics
import subprocess


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def workspace_view(corpus, directory):
    """Expose every frozen recursive source to the existing flat replay reader.

    Preserve the thirteen active-file names. Encode nested paths explicitly and
    record the mapping, rather than silently omitting nested source modules.
    """
    manifest = json.loads((corpus / "manifest.json").read_text(encoding="utf-8"))
    directory.mkdir(parents=True, exist_ok=True)
    mapping = {}
    for entry in manifest["files"]:
        relative = Path(entry["path"]).relative_to("src")
        name = relative.name if len(relative.parts) == 1 else (
            "__nested_" + hashlib.sha256(entry["path"].encode()).hexdigest()[:16] + "_" + relative.name)
        assert name not in mapping, "flattened workspace name collision"
        original = corpus / entry["path"]
        assert digest(original) == entry["sha256"]
        destination = directory / name
        if destination.exists():
            assert digest(destination) == entry["sha256"], "frozen workspace view changed"
        else:
            destination.write_bytes(original.read_bytes())
        mapping[name] = entry
    assert len(list(directory.glob("*.rs"))) == len(manifest["files"])
    (directory / "manifest.json").write_text(json.dumps(mapping, indent=2) + "\n", encoding="utf-8")
    return directory


def fixture(report, workload):
    keys = ("group", "root", "positive", "visible", "prefix", "expected", "stem") if workload == "holdout" else ("file", "offset", "expected")
    rows = [{key: row[key] for key in keys} for row in report["rows"]]
    return hashlib.sha256(json.dumps(rows, sort_keys=True, ensure_ascii=False).encode("utf-8")).hexdigest()


def quality(report, workload):
    keys = ["top1", "top5", "candidateRecall", "negativeCases"]
    keys += ["positiveCases", "negativeUnwantedCases"] if workload == "holdout" else ["unwanted", "limited"]
    return {key: report[key] for key in keys}


def groups(report):
    result = {}
    for row in report["rows"]:
        group = result.setdefault(row["group"], dict(cases=0, top1=0, top5=0, candidateRecall=0, safeStem=0, novelAbstentions=0, unwantedNovel=0))
        group["cases"] += 1
        group["top1"] += row["rank"] == 0
        group["top5"] += row["rank"] is not None and row["rank"] < 5
        group["candidateRecall"] += row["candidateRecall"]
        group["safeStem"] += row["stemRank"] is not None
        group["novelAbstentions"] += row["rank"] is None and row["stemRank"] is None and not row["unwantedNovel"]
        group["unwantedNovel"] += bool(row["unwantedNovel"])
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--workspace", action="store_true", help="index every frozen source file in native replay sessions")
    args = parser.parse_args()
    assert args.repeats >= 1
    args.output.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment["VTCODE_GENERATION_MODE"] = "full"
    environment["VTCODE_GENERATION_SPLIT"] = "holdout"
    environment["VTCODE_GENERATION_CORPUS"] = str(args.corpus.resolve())
    source = workspace_view(args.corpus, args.output / "workspace-source") if args.workspace else args.corpus / "src"
    environment["VTCODE_CAPACITY_SOURCE"] = str(source.resolve())
    if args.workspace:
        environment["VTCODE_VTCODE_REPLAY_WORKSPACE"] = "1"
    else:
        environment.pop("VTCODE_VTCODE_REPLAY_WORKSPACE", None)
    binaries = {"before": args.baseline.resolve(), "after": args.candidate.resolve()}
    summary = dict(corpusManifestSha256=digest(args.corpus / "manifest.json"),
                   binaries={name: dict(name=path.name, sha256=digest(path)) for name, path in binaries.items()}, workloads={})
    if args.workspace:
        summary["workspaceViewManifestSha256"] = digest(source / "manifest.json")
        summary["workspaceFiles"] = len(json.loads((source / "manifest.json").read_text(encoding="utf-8")))
    for workload, test, variable in [
        ("holdout", "benchmark_generated_identifier_replay", "VTCODE_GENERATION_REPORT"),
        ("native", "benchmark_vtcode_editing_replay", "VTCODE_VTCODE_REPLAY_REPORT"),
    ]:
        reports = {"before": [], "after": []}
        expected_fixture = None
        for repeat in range(args.repeats):
            order = ("before", "after") if repeat % 2 == 0 else ("after", "before")
            for name in order:
                stem = f"{workload}-{repeat + 1}-{name}"
                output = args.output / f"{stem}.json"
                environment[variable] = str(output.resolve())
                with (args.output / f"{stem}.log").open("w", encoding="utf-8") as log:
                    subprocess.run([str(binaries[name]), test, "--ignored", "--nocapture", "--test-threads=1"],
                                   env=environment, stdout=log, stderr=subprocess.STDOUT, check=True)
                report = json.loads(output.read_text(encoding="utf-8"))
                current_fixture = fixture(report, workload)
                if expected_fixture is None:
                    expected_fixture = current_fixture
                assert current_fixture == expected_fixture, "baseline and candidate received different inputs"
                if reports[name]:
                    assert quality(report, workload) == quality(reports[name][0], workload), "quality changed across repeats"
                reports[name].append(report)
                print(stem, quality(report, workload), "worker p95 us", report["workerWarmP95Us"], flush=True)
        measured = dict(fixtureSha256=expected_fixture, cases=len(reports["before"][0]["rows"]))
        for name, runs in reports.items():
            latency_keys = [key for key in runs[0] if key.endswith("Us")]
            measured[name] = dict(quality=quality(runs[0], workload),
                                 latencyUs={key: dict(runs=[run[key] for run in runs], median=statistics.median(run[key] for run in runs)) for key in latency_keys},
                                 completionEvidenceBytes=runs[0].get("completionEvidenceBytes"))
            if workload == "holdout":
                measured[name]["groups"] = groups(runs[0])
        if workload == "native":
            pairs = list(zip(reports["before"][0]["rows"], reports["after"][0]["rows"]))
            measured["rankChangedCases"] = sum(a["rank"] != b["rank"] for a, b in pairs)
            measured["candidateRecallChangedCases"] = sum(a["recalled"] != b["recalled"] for a, b in pairs)
        summary["workloads"][workload] = measured
    novel = summary["workloads"]["holdout"]["after"]["quality"]
    native = summary["workloads"]["native"]
    count = native["cases"]
    summary["acceptance"] = dict(
        novelTop1AtLeast70Percent=novel["top1"] / novel["positiveCases"] >= .7,
        noUnwantedNovel=novel["negativeUnwantedCases"] == 0,
        ordinaryTop1LossWithinOnePoint=(native["before"]["quality"]["top1"] - native["after"]["quality"]["top1"]) / count <= .01,
        ordinaryTop5LossWithinOnePoint=(native["before"]["quality"]["top5"] - native["after"]["quality"]["top5"]) / count <= .01,
        noExtraImpossiblePrefixPopups=native["after"]["quality"]["unwanted"] <= native["before"]["quality"]["unwanted"],
        everyMeasuredWarmWorkerP95Below5Ms=all(
            value < 5000 for workload in summary["workloads"].values() for name in binaries
            for value in workload[name]["latencyUs"]["workerWarmP95Us"]["runs"]),
    )
    path = args.output / "summary.json"
    path.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    print(path)
    assert all(summary["acceptance"].values()), summary["acceptance"]


if __name__ == "__main__":
    main()
