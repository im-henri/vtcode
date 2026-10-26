"""Timing reports must distinguish skipped, failed, and interrupted tests."""

import contextlib
import csv
import io
import json
from pathlib import Path
import tempfile
import unittest

from scripts import test_timings


class TimingReportTests(unittest.TestCase):
    def report(self, events, encoding="utf-8"):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        folder = Path(temporary.name)
        log = folder / "native.jsonl"
        log.write_text("peer diagnostic\n" + "\n".join(json.dumps(event) for event in events), encoding=encoding)
        with contextlib.redirect_stdout(io.StringIO()):
            result = test_timings.report([log], folder)
        with (folder / "tests.csv").open(newline="", encoding="utf-8") as stream:
            rows = list(csv.DictReader(stream))
        return result, rows

    def test_native_timings_keep_failures_skips_and_target_identity(self):
        for encoding in ("utf-8", "utf-16"):
            with self.subTest(encoding=encoding):
                summary, rows = self.report([
                    {"type": "suite", "event": "started", "test_count": 3},
                    {"type": "test", "event": "started", "name": "fast"},
                    {"type": "test", "event": "ok", "name": "fast", "exec_time": .012},
                    {"type": "test", "event": "failed", "name": "broken", "exec_time": 1.25},
                    {"type": "test", "event": "ignored", "name": "manual"},
                    {"type": "suite", "event": "failed", "exec_time": 1.3},
                ], encoding)
                self.assertEqual((summary["passed"], summary["failed"], summary["ignored"]), (1, 1, 1))
                self.assertEqual([row["name"] for row in rows], ["broken", "fast", "manual"])
                self.assertTrue(all(row["target"] == "native" for row in rows))
                self.assertEqual(rows[-1]["seconds"], "")

    def test_empty_or_interrupted_runner_cannot_report_success(self):
        for events in ([], [
            {"type": "suite", "event": "started", "test_count": 2},
            {"type": "test", "event": "started", "name": "hung"},
        ]):
            with self.subTest(events=events):
                summary, rows = self.report(events)
                self.assertGreater(summary["failed"], 0)
                self.assertTrue(all(row["status"] == "incomplete" for row in rows))
