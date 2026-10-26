"""Deterministic link scheduling plus real PTY/CPU/disk fault-harness checks."""
import json
import os
import signal
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

from scripts.bridge_lab import Link, build_disk_shim

ROOT = Path(__file__).resolve().parents[2]
LAB = ROOT / "scripts/bridge_lab.py"


class LinkTests(unittest.TestCase):
    def test_latency_is_not_added_again_per_chunk_and_order_survives_jitter(self):
        link = Link(latency=0.04, jitter=0.03, seed=7)
        for byte in range(100):
            link.put(bytes([byte]), 1)
        self.assertLessEqual(link.tail, 1.07)
        self.assertFalse(link.ready(1))
        received = bytearray()
        with patch("os.write", side_effect=lambda _, data: (received.extend(data), len(data))[1]):
            while link.ready(2):
                link.write(9, 2)
        self.assertEqual(received, bytes(range(100)))
        self.assertEqual(link.pending, 0)
        self.assertEqual(link.received, link.transferred)

    def test_bandwidth_is_bounded_and_bidirectional_links_are_independent(self):
        output = Link(latency=0.05, rate=100)
        incoming = Link(latency=0.05, rate=100)
        output.put(bytes(100), 1)
        output.put(bytes(100), 1)
        incoming.put(b"key", 1)
        self.assertAlmostEqual(output.tail, 3.05)
        self.assertAlmostEqual(incoming.tail, 1.08)
        self.assertTrue(incoming.ready(1.1))
        self.assertFalse(output.ready(1.1))

    def test_partial_writes_backpressure_and_capacity_do_not_drop_bytes(self):
        link = Link(capacity=5)
        link.put(b"abcde", 0)
        with self.assertRaises(BufferError):
            link.put(b"f", 0)
        with patch("os.write", side_effect=BlockingIOError):
            link.write(9, 0)
        self.assertEqual(link.pending, 5)
        with patch("os.write", return_value=2):
            link.write(9, 0)
        self.assertEqual(bytes(link.queue[0][1]), b"cde")
        self.assertEqual(link.available, 2)
        link.put(b"fg", 0)
        self.assertEqual(link.peak, 5)
        self.assertEqual(link.metrics()["transferred"], 2)

    def test_invalid_limits_and_empty_messages(self):
        for params in ({"rate": -1}, {"capacity": 0}, {"latency": -0.1}):
            with self.assertRaises(ValueError):
                Link(**params)
        link = Link()
        link.put(b"", 0)
        self.assertFalse(link.ready(100))


@unittest.skipUnless(sys.platform == "linux", "Linux PTY, affinity and preload tests")
class LinuxLabTests(unittest.TestCase):
    def run_lab(self, command, *options, data=b"", timeout=15):
        with tempfile.TemporaryDirectory() as directory:
            metrics = Path(directory) / "metrics.json"
            result = subprocess.run([sys.executable, str(LAB), "--timeout", "10", *options, "--metrics", str(metrics),
                                     "--", *command], input=data, capture_output=True, timeout=timeout)
            return result, json.loads(metrics.read_text())

    def test_workload_cpu_measurement_excludes_the_load_generator(self):
        probe = "import time; end=time.process_time()+.1\nwhile time.process_time()<end: pass\ntime.sleep(.5)"
        result, metrics = self.run_lab([sys.executable, "-c", probe], "--single-core", "--cpu-load-percent", "70")
        self.assertEqual(result.returncode, 0, result.stderr)
        usage = metrics["workload"]
        cpu = usage["user_cpu_seconds"] + usage["system_cpu_seconds"]
        self.assertGreaterEqual(cpu, .08)
        self.assertLess(cpu, .4, "The synthetic CPU contender must not be counted as editor work")
        self.assertGreater(usage["peak_rss_kib"], 0)
        self.assertGreaterEqual(metrics["wrapper_cpu_seconds"], 0)

    def test_fragmented_slow_link_preserves_unicode_ansi_and_full_duplex_bulk(self):
        data = ("🦀é界\u0301\x1b[?777;1;Mtest~\x1b[13;2u" * 100).encode()
        probe = "import os;data=b'';\nwhile len(data)<%d:data+=os.read(0,65536)\nos.write(1,data)" % len(data)
        result, metrics = self.run_lab([sys.executable, "-c", probe], "--latency-ms", "5",
                                      "--jitter-ms", "3", "--bytes-per-second", "65536",
                                      "--chunk-size", "1", "--max-buffer", "512", data=data)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, data)
        self.assertEqual(metrics["input"]["transferred"], len(data))
        self.assertEqual(metrics["output"]["transferred"], len(data))
        self.assertLessEqual(metrics["output"]["peak"], 512)
        self.assertEqual(metrics["reason"], "exit")

    def test_large_output_backpressure_preserves_all_bytes(self):
        data = b"x" * (256 * 1024)
        probe = "import os,tty;tty.setraw(0);data=b'x'*262144;\nwhile data:data=data[os.write(1,data):]"
        result, metrics = self.run_lab([sys.executable, "-c", probe], "--max-buffer", "4096",
                                      "--bytes-per-second", "1048576")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, data)
        self.assertEqual(metrics["output"]["transferred"], len(data))
        self.assertLessEqual(metrics["output"]["peak"], 4096)

    def test_cpu_constraint_exit_status_and_clean_timeout(self):
        probe = "import os;print('CPUS',len(os.sched_getaffinity(0)));raise SystemExit(7)"
        result, metrics = self.run_lab([sys.executable, "-c", probe], "--single-core", "--cpu-load-percent", "70")
        self.assertEqual(result.returncode, 7, result.stderr)
        self.assertIn(b"CPUS 1", result.stdout)
        self.assertEqual(metrics["exit_status"], 7)
        result, metrics = self.run_lab([sys.executable, "-c", "import time;time.sleep(60)"], "--timeout", "0.1")
        self.assertEqual(result.returncode, 124)
        self.assertEqual(metrics["reason"], "timeout")

    def test_stall_recovery_and_disconnect_are_bounded(self):
        probe = "import os,tty;tty.setraw(0);os.write(1,b'OK')"
        result, metrics = self.run_lab([sys.executable, "-c", probe], "--stall-ms", "150")
        self.assertEqual(result.stdout, b"OK")
        self.assertGreaterEqual(metrics["elapsed_seconds"], 0.15)
        result, metrics = self.run_lab([sys.executable, "-c", "import time;time.sleep(60)"],
                                      "--disconnect-after-ms", "100")
        self.assertEqual(result.returncode, 124)
        self.assertEqual(metrics["reason"], "disconnect")

    def test_closed_stdout_cannot_defeat_timeout(self):
        result, metrics = self.run_lab([sys.executable, "-c", "import os,time;os.close(1);os.close(2);time.sleep(60)"],
                                      "--timeout", "0.1")
        self.assertEqual(result.returncode, 124)
        self.assertEqual(metrics["reason"], "timeout")

    def test_hangup_stops_the_workload_and_cpu_contender_and_writes_metrics(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "started"
            metrics = Path(directory) / "metrics.json"
            probe = "from pathlib import Path; import time; Path(%r).write_text('ready'); time.sleep(60)" % str(marker)
            process = subprocess.Popen([sys.executable, str(LAB), "--single-core", "--cpu-load-percent", "70",
                                        "--metrics", str(metrics), "--", sys.executable, "-c", probe],
                                       stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            children = {}
            try:
                end = time.monotonic() + 5
                while time.monotonic() < end:
                    path = Path(f"/proc/{process.pid}/task/{process.pid}/children")
                    for pid in map(int, path.read_text().split()):
                        stat = Path(f"/proc/{pid}/stat").read_text().rsplit(')', 1)[1].split()
                        children[pid] = stat[19]
                    if marker.exists() and len(children) == 2:
                        break
                    time.sleep(.01)
                self.assertEqual(len(children), 2, "Both the workload and CPU contender must start")
                process.send_signal(signal.SIGHUP)
                output, errors = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 130, (output, errors))
                self.assertEqual(json.loads(metrics.read_text())["reason"], "cancelled")
                for pid in children:
                    path = Path(f"/proc/{pid}/stat")
                    self.assertFalse(path.exists() and path.read_text().rsplit(')', 1)[1].split()[0] != "Z",
                                     f"Owned test process {pid} survived hangup")
            finally:
                if process.poll() is None:
                    process.kill()
                    process.communicate(timeout=5)
                # A failing regression must not leave its own load behind.
                for pid, started in children.items():
                    try:
                        if Path(f"/proc/{pid}/stat").read_text().rsplit(')', 1)[1].split()[19] == started:
                            os.kill(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    except FileNotFoundError:
                        pass

    def test_slow_disk_applies_to_only_canonical_workspace_and_preserves_data(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "workspace"
            root.mkdir()
            other = Path(directory) / "workspace-other"
            other.mkdir()
            (root / "escape").symlink_to(other, target_is_directory=True)
            shim = build_disk_shim(directory)
            env = {**os.environ, "LD_PRELOAD": str(shim), "VTCODE_LAB_DISK_ROOT": str(root),
                   "VTCODE_LAB_DISK_DELAY_US": "20000"}
            probe = """import os,sys,time,ctypes
start=time.monotonic()
fd=os.open(sys.argv[1],os.O_RDWR|os.O_CREAT,0o600)
statx=ctypes.CDLL(None,use_errno=True).statx
statx.argtypes=[ctypes.c_int,ctypes.c_char_p,ctypes.c_int,ctypes.c_uint,ctypes.c_void_p]
statx.restype=ctypes.c_int
# Empty strings support AT_EMPTY_PATH on every glibc baseline. Older headers
# declare the path non-null, so a null pointer can be optimized into a crash.
assert statx(fd,b'',0x1000,0x7ff,ctypes.create_string_buffer(256))==0,ctypes.get_errno()
os.write(fd,b'new text')
os.fsync(fd)
os.lseek(fd,0,0)
assert os.read(fd,128)==b'new text'
os.close(fd)
print(time.monotonic()-start)
"""
            elapsed = []
            for path in (root / "file", other / "file", root / "escape/file", root / "../workspace-other/file"):
                result = subprocess.run([sys.executable, "-c", probe, str(path)], env=env,
                                        capture_output=True, timeout=10, check=True)
                elapsed.append(float(result.stdout))
            self.assertGreaterEqual(elapsed[0], 0.075)
            self.assertTrue(all(value < elapsed[0] / 2 for value in elapsed[1:]), elapsed)
            self.assertEqual((root / "file").read_bytes(), b"new text")


if __name__ == "__main__":
    unittest.main()
