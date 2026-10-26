"""Provenance checks must reject a substituted executable or source revision."""
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import benchmark_completion_advanced as benchmark


class BaselineProvenanceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.binary = self.root / 'baseline.exe'
        self.binary.write_bytes(b'preserved test executable')
        self.source = b'fn completion() {}\n'
        self.expected = hashlib.sha256(self.source).hexdigest()
        self.record = self.root / 'provenance.json'

    def write_record(self, old=False, source_hash=None):
        production = {
            'src/completion.rs' if old else str(self.root / 'src/completion.rs'):
                source_hash or self.expected,
        }
        binary_hash = hashlib.sha256(self.binary.read_bytes()).hexdigest()
        value = {'provenance': {'candidateBinarySha256': binary_hash, 'productionFiles': production}} if old else {
            'binaries': {'candidate': {'sha256': binary_hash}}, 'productionFiles': production,
        }
        self.record.write_text(json.dumps(value), encoding='utf-8')

    def verify(self, output=None):
        with patch.object(benchmark, 'ROOT', self.root), patch.object(
                benchmark.subprocess, 'check_output', return_value=output or self.source) as git:
            result = benchmark.verify_baseline(self.binary, 'exact-revision', self.record)
        git.assert_called_once_with(['git', 'show', 'exact-revision:src/completion.rs'], cwd=self.root)
        return result

    def test_both_report_formats_verify_all_recorded_sources(self):
        for old in (False, True):
            with self.subTest(old=old):
                self.write_record(old)
                self.assertEqual(self.verify(), ['src/completion.rs'])

    def test_only_checkout_line_endings_can_differ(self):
        self.write_record()
        self.assertEqual(self.verify(self.source.replace(b'\n', b'\r\n')), ['src/completion.rs'])

    def test_wrong_executable_is_rejected_before_reading_git(self):
        self.write_record()
        self.binary.write_bytes(b'substituted executable')
        with patch.object(benchmark, 'ROOT', self.root), patch.object(
                benchmark.subprocess, 'check_output') as git, self.assertRaises(AssertionError):
            benchmark.verify_baseline(self.binary, 'exact-revision', self.record)
        git.assert_not_called()

    def test_changed_source_is_rejected(self):
        self.write_record(source_hash=hashlib.sha256(b'different source').hexdigest())
        with self.assertRaises(AssertionError):
            self.verify()

    def test_source_outside_repository_is_rejected(self):
        self.write_record()
        value = json.loads(self.record.read_text())
        value['productionFiles'] = {'../outside.rs': self.expected}
        self.record.write_text(json.dumps(value))
        with patch.object(benchmark, 'ROOT', self.root), self.assertRaises(AssertionError):
            benchmark.verify_baseline(self.binary, 'exact-revision', self.record)


if __name__ == '__main__':
    unittest.main()
