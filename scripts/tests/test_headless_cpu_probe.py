import argparse
import unittest
from unittest.mock import patch

from scripts.headless_cpu_probe import Counters, Sample, main, parse_stat, positive_seconds, report


def sample(when, ticks, threads=None, start=99, io=None):
    return Sample(when, Counters('vtcode', start, ticks), threads or {}, {}, io or {})


class HeadlessCpuProbeTests(unittest.TestCase):
    def test_stat_parser_preserves_names_with_spaces_and_parentheses(self):
        fields = ['S'] + ['0'] * 49
        fields[11], fields[12], fields[19] = '7', '8', '1234'
        self.assertEqual(parse_stat('42 (headless (git)) ' + ' '.join(fields)),
                         Counters('headless (git)', 1234, 15))

    def test_interval_usage_measures_2_point_5_percent_without_startup_average(self):
        before = sample(10, 9000, {1: Counters('headless-git', 100, 8000)})
        after = sample(40, 9075, {1: Counters('headless-git', 100, 8075)})
        result = report(42, before, after, 100)
        self.assertEqual(result['cpu_percent_one_core'], 2.5)
        self.assertEqual(result['cpu_ms'], 750)
        self.assertEqual(result['threads'][0]['cpu_percent_one_core'], 2.5)
        self.assertEqual(result['cpu_resolution_ms'], 10)

    def test_quiet_process_has_no_charged_cpu(self):
        result = report(42, sample(10, 700), sample(40, 700), 100)
        self.assertEqual(result['cpu_ms'], 0)
        self.assertEqual(result['cpu_percent_one_core'], 0)

    def test_thread_churn_does_not_lose_process_cpu_or_misattribute_reused_tids(self):
        before = sample(10, 100, {1: Counters('finished', 10, 10), 2: Counters('old', 20, 50)})
        after = sample(40, 175, {2: Counters('new', 40, 5), 3: Counters('created', 30, 5)})
        result = report(42, before, after, 100)
        self.assertEqual(result['cpu_ms'], 750)
        self.assertEqual(result['threads_ended'], [1])
        for thread in result['threads']:
            self.assertFalse(thread['complete_interval'])
            self.assertIsNone(thread['cpu_ms'])

    def test_pid_reuse_invalidates_measurement(self):
        with self.assertRaisesRegex(ProcessLookupError, 'reused'):
            report(42, sample(10, 100), sample(40, 175, start=101), 100)

    def test_invalid_intervals_and_backwards_counters_are_rejected(self):
        for after, hz in [(sample(10, 100), 100), (sample(40, 99), 100), (sample(40, 101), 0)]:
            with self.assertRaises(ValueError):
                report(42, sample(10, 100), after, hz)
        for text in ['0', '-1', 'nan', 'inf']:
            with self.assertRaises(argparse.ArgumentTypeError):
                positive_seconds(text)

    def test_io_is_interval_metadata_not_file_contents(self):
        result = report(42, sample(10, 100, io={'rchar': 100, 'syscr': 10}),
                        sample(40, 101, io={'rchar': 120, 'syscr': 11, 'wchar': 200}), 100)
        self.assertEqual(result['io_delta'], {'rchar': 20, 'syscr': 1})

    @patch('scripts.headless_cpu_probe.sys.platform', 'linux')
    @patch('scripts.headless_cpu_probe.sys.argv', ['headless_cpu_probe.py', '123', '--seconds', '.01'])
    @patch('scripts.headless_cpu_probe.time.sleep')
    @patch('scripts.headless_cpu_probe.read_sample', side_effect=FileNotFoundError)
    @patch('scripts.headless_cpu_probe.sys.stderr')
    def test_dead_process_fails_without_reporting_zero_cpu(self, stderr, read, sleep):
        self.assertEqual(main(), 1)
        sleep.assert_not_called()
        self.assertTrue(stderr.write.called)


if __name__ == '__main__':
    unittest.main()
