#!/usr/bin/env python3
"""Run VTCode's opt-in editor workloads under reproducible Linux fault profiles.

python3 scripts/bridge_lab_suite.py --test-exe /path/to/vtcode-test-executable \
    --output target/bridge-lab

Build the test executable first (`cargo test --bin vtcode --no-run`). Workloads
use temporary files and test app state, never the user's editor configuration.
Network input fidelity is exercised separately by the SSH integration fixture.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
PROFILES = {
    "native": [],
    "single-core-loaded": ["--single-core", "--cpu-load-percent", "70"],
    "slow-disk": ["--disk-delay-ms", "20"],
    "combined": ["--single-core", "--cpu-load-percent", "70", "--disk-delay-ms", "20",
                 "--latency-ms", "40", "--jitter-ms", "10", "--bytes-per-second", "32768"],
}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--test-exe", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--profiles", nargs="+", choices=list(PROFILES), default=list(PROFILES))
    args = parser.parse_args()
    if sys.platform != "linux":
        parser.error("run this suite on Linux, including the remote compute node")
    executable = args.test_exe.resolve(strict=True)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    results = []
    for name in args.profiles:
        with tempfile.TemporaryDirectory(prefix="vtcode-lab-workspace-") as workspace:
            metrics = output / f"{name}-transport.json"
            environment = dict(os.environ, VTCODE_HEADLESS_LAB_ROOT=workspace,
                               VTCODE_HEADLESS_LAB_OPTIONS=json.dumps(PROFILES[name]),
                               VTCODE_HEADLESS_LAB_METRICS=str(metrics))
            command = [str(executable), "--exact", "remote::tests::headless_latency_benchmark",
                       "--ignored", "--nocapture", "--test-threads=1"]
            if name in ("single-core-loaded", "combined"):
                # Constrain the real workbench too. Its headless child inherits
                # this CPU; use one shared load generator instead of two.
                options = PROFILES[name].copy()
                options.remove("--single-core")
                index = options.index("--cpu-load-percent")
                del options[index:index + 2]
                environment["VTCODE_HEADLESS_LAB_OPTIONS"] = json.dumps(options)
                command = [sys.executable, str(ROOT / "scripts/bridge_lab.py"), "--single-core",
                           "--cpu-load-percent", "70", "--timeout", "145", "--", *command]
            completed = subprocess.run(command, capture_output=True, timeout=150, env=environment, cwd=ROOT)
            text = completed.stdout.decode("utf-8", errors="replace")
            (output / f"{name}.log").write_bytes(completed.stdout + completed.stderr)
            if completed.returncode:
                raise RuntimeError(f"{name} failed ({completed.returncode}): {text}\n{completed.stderr.decode(errors='replace')}")
            marker = "VTCODE_HEADLESS_LAB "
            measurement = next(json.loads(line.split(marker, 1)[1]) for line in text.splitlines() if marker in line)
            results.append({"profile": name, "editor": measurement, "transport": json.loads(metrics.read_text())})
            print(json.dumps({"profile": name, **measurement}), flush=True)
    report = {"profiles": results}
    (output / "results.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
