#!/usr/bin/env python3
"""Measure individual local tests; keep compilation separate from test runtime.

Run: python scripts/test_timings.py run --output target/test-timings
Add --sdk for the official local Python peer checks used by local.py.
Existing libtest JSON logs: python scripts/test_timings.py summarize DIRECTORY
"""

from __future__ import annotations

import argparse
import csv
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import unittest

if __package__:
    from . import local
else:
    import local


ROOT = Path(__file__).resolve().parents[1]


def events(path):
    data = path.read_bytes()
    encoding = "utf-16" if data.startswith((b"\xff\xfe", b"\xfe\xff")) else "utf-8-sig"
    for line in data.decode(encoding).splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue  # Native peer/compiler diagnostics are not libtest events.
        if isinstance(event, dict):
            yield event


def report(paths, output):
    rows, suites = [], []
    for path in paths:
        started, finished = set(), set()
        expected, complete = None, False
        for event in events(path):
            if event.get("type") == "suite" and event.get("event") == "started":
                expected = event["test_count"]
            if event.get("type") == "suite" and event.get("event") in ("ok", "failed"):
                complete = True
                suites.append({"target": path.stem, **event})
            if event.get("type") != "test":
                continue
            name, status = event["name"], event["event"]
            if status == "started":
                started.add(name)
            elif status in ("ok", "failed", "ignored"):
                finished.add(name)
                rows.append({"target": path.stem, "name": name, "status": status,
                             "seconds": event.get("exec_time")})
        rows.extend({"target": path.stem, "name": name, "status": "incomplete", "seconds": None}
                    for name in sorted(started - finished))
        if not complete or expected != len(finished):
            rows.append({"target": path.stem, "name": "<runner>", "status": "incomplete", "seconds": None})
    rows.sort(key=lambda row: (-(row["seconds"] or 0), row["target"], row["name"]))
    output.mkdir(parents=True, exist_ok=True)
    with (output / "tests.csv").open("w", encoding="utf-8", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=("target", "name", "status", "seconds"))
        writer.writeheader()
        writer.writerows(rows)
    groups = {}
    for row in rows:
        key = f"{row['target']}::{row['name'].split('::')[0]}"
        group = groups.setdefault(key, {"tests": 0, "seconds": 0.0})
        if row["status"] != "ignored":
            group["tests"] += 1
            group["seconds"] += row["seconds"] or 0
    summary = {"suites": suites, "tests": len(rows),
               "passed": sum(row["status"] == "ok" for row in rows),
               "failed": sum(row["status"] in ("failed", "incomplete") for row in rows),
               "ignored": sum(row["status"] == "ignored" for row in rows),
               "groups": dict(sorted(groups.items(), key=lambda item: -item[1]["seconds"])),
               "slowest": rows[:30]}
    local.write_json(output / "summary.json", summary)
    for row in rows[:20]:
        print(f"{row['seconds'] or 0:8.3f}s  {row['target']}::{row['name']}")
    print(f"{summary['passed']} passed, {summary['failed']} failed/incomplete, "
          f"{summary['ignored']} ignored. Per-test timings: {output / 'tests.csv'}")
    return summary


def cargo_test(manifest, *arguments):
    # The application commits its lockfile; extension-api is a separately
    # published library whose generated lockfile is intentionally ignored.
    locked = ["--locked"] if manifest == "Cargo.toml" else []
    return ["cargo", "test", "--manifest-path", manifest, *locked, *arguments]


def artifacts(manifest, env):
    command = cargo_test(manifest, "--all-targets", "--no-run", "--message-format=json")
    print("+ " + " ".join(str(arg) for arg in command), flush=True)
    result = subprocess.run(command, cwd=ROOT, env=env, text=True, encoding="utf-8",
                            stdout=subprocess.PIPE)
    if result.returncode:
        for line in result.stdout.splitlines():
            event = json.loads(line)
            if event.get("reason") == "compiler-message":
                print(event["message"].get("rendered", event["message"]["message"]), file=sys.stderr)
    result.check_returncode()
    binaries = {}
    for line in result.stdout.splitlines():
        event = json.loads(line)
        if (event.get("reason") == "compiler-artifact" and event.get("executable")
                and event["profile"]["test"]):
            target = event["target"]
            label = f"{target['name']}-{target['kind'][0]}"
            binaries[label] = event["executable"]
    return binaries


def rust_run(binary, path, env, threads, filters=(), ignored=False):
    # Only the test process needs libtest's experimental JSON reporter. The
    # compiler/build environment above stays on the pinned stable toolchain.
    test_env = {**env, "RUSTC_BOOTSTRAP": "1"}
    command = [binary, *filters, "--test-threads", str(threads), "--report-time",
               "--format=json", "-Z", "unstable-options"]
    if ignored:
        command.append("--ignored")
    print(f"Timing {path.stem} ...", flush=True)
    with path.open("w", encoding="utf-8") as stream:
        result = subprocess.run(command, cwd=ROOT, env=test_env, stdout=stream,
                                stderr=subprocess.STDOUT)
    return result.returncode


def python_run(path, pattern="test*.py"):
    # Direct script invocation puts scripts/ rather than the repository root
    # on sys.path. Tests also import scripts as a package, as `python -m` does.
    if str(ROOT) not in sys.path:
        sys.path.insert(0, str(ROOT))
    with path.open("w", encoding="utf-8") as stream:
        class TimedResult(unittest.TextTestResult):
            def startTest(self, test):
                super().startTest(test)
                self.started = time.perf_counter()
                self.status = "ok"
                stream.write(json.dumps({"type": "test", "event": "started", "name": test.id()}) + "\n")

            def addFailure(self, test, error):
                self.status = "failed"
                super().addFailure(test, error)

            def addError(self, test, error):
                self.status = "failed"
                super().addError(test, error)

            def addSubTest(self, test, subtest, error):
                if error is not None:
                    self.status = "failed"
                super().addSubTest(test, subtest, error)

            def addSkip(self, test, reason):
                self.status = "ignored"
                super().addSkip(test, reason)

            def stopTest(self, test):
                stream.write(json.dumps({"type": "test", "event": self.status, "name": test.id(),
                                         "exec_time": time.perf_counter() - self.started}) + "\n")
                super().stopTest(test)

        suite = unittest.defaultTestLoader.discover(str(ROOT / "scripts/tests"), pattern=pattern)
        stream.write(json.dumps({"type": "suite", "event": "started", "test_count": suite.countTestCases()}) + "\n")
        started = time.perf_counter()
        result = unittest.TextTestRunner(resultclass=TimedResult).run(suite)
        stream.write(json.dumps({"type": "suite", "event": "ok" if result.wasSuccessful() else "failed",
                                 "exec_time": time.perf_counter() - started}) + "\n")
        return 0 if result.wasSuccessful() else 1


def run(args):
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    env = local.environment()
    if args.sdk:
        local.check_python(env)
    build_started = time.perf_counter()
    binaries = artifacts("Cargo.toml", env)
    binaries.update(artifacts("extension-api/Cargo.toml", env))
    context = {"threads": args.threads, "sdk": args.sdk,
               "buildSeconds": time.perf_counter() - build_started,
               "artifacts": binaries, **local.source_state()}
    local.write_json(output / "context.json", context)
    logs, exits = [], []
    for label, binary in binaries.items():
        path = output / f"{label}.jsonl"
        exits.append(rust_run(binary, path, env, args.threads))
        logs.append(path)
        if args.sdk and label in ("acp_integration-test", "vtcode-bin"):
            path = output / f"{label}-sdk.jsonl"
            filters = local.ACP_FILTERS if label == "vtcode-bin" else ()
            exits.append(rust_run(binary, path, env, 1, filters, ignored=True))
            logs.append(path)
    path = output / "python.jsonl"
    exits.append(python_run(path))
    logs.append(path)
    summary = report(logs, output)
    # Cargo test also runs doctests, which have their own harness and need not
    # support libtest JSON. Keep them in validation and report their wall time.
    for manifest in ("Cargo.toml", "extension-api/Cargo.toml"):
        started = time.perf_counter()
        result = subprocess.run(cargo_test(manifest, "--doc"),
                                cwd=ROOT, env=env)
        exits.append(result.returncode)
        context.setdefault("doctests", []).append({"manifest": manifest,
                                                   "seconds": time.perf_counter() - started,
                                                   "exitCode": result.returncode})
    context["exitCodes"] = exits
    context["sourcesUnchanged"] = all(context[key] == value for key, value in local.source_state().items())
    local.write_json(output / "context.json", context)
    return int(any(exits) or summary["failed"] > 0 or not context["sourcesUnchanged"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    collect = commands.add_parser("run")
    collect.add_argument("--output", type=Path, default=ROOT / "target/test-timings")
    collect.add_argument("--threads", type=int, default=2)
    collect.add_argument("--sdk", action="store_true")
    summarize = commands.add_parser("summarize")
    summarize.add_argument("directory", type=Path)
    args = parser.parse_args()
    if args.command == "summarize":
        paths = sorted(args.directory.glob("*.jsonl"))
        if not paths:
            parser.error("No libtest JSON logs found")
        return int(report(paths, args.directory)["failed"] > 0)
    if args.threads < 1:
        parser.error("--threads must be positive")
    return run(args)


if __name__ == "__main__":
    sys.exit(main())
