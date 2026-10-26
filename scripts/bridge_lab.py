#!/usr/bin/env python3
"""Repeatable, unprivileged terminal fault injection for VTCode and its bridge.

Linux: python3 scripts/bridge_lab.py --latency-ms 40 --bytes-per-second 32768 \
    --single-core --cpu-load-percent 70 --metrics /tmp/bridge.json -- vtcode

Place this inside an interactive SSH/job session. It passes an arbitrary command
through a PTY; it has no knowledge of VTCode's wire protocol or scheduler aliases.
The clock/queue core is portable and unit tested on Windows too.
"""
from __future__ import annotations

import argparse
from collections import deque
from contextlib import contextmanager
from dataclasses import dataclass, field
import errno
import json
import os
from pathlib import Path
import random
import select
import signal
import subprocess
import sys
import tempfile
import time


@dataclass
class Link:
    """Ordered, bounded byte transport with independent scheduling per direction.

    Latency delays delivery, not the reader. Bandwidth is serialized without
    sleeping per packet; slow output cannot prevent input or SIGWINCH handling.
    """
    latency: float = 0
    jitter: float = 0
    rate: int = 0
    capacity: int = 1024 * 1024
    seed: int = 1
    queue: deque = field(default_factory=deque, init=False)
    pending: int = field(default=0, init=False)
    peak: int = field(default=0, init=False)
    received: int = field(default=0, init=False)
    transferred: int = field(default=0, init=False)
    writes: int = field(default=0, init=False)
    tail: float = field(default=0, init=False)

    def __post_init__(self):
        if min(self.latency, self.jitter, self.rate) < 0 or self.capacity < 1:
            raise ValueError("link limits must be nonnegative; capacity must be positive")
        self.random = random.Random(self.seed)

    @property
    def available(self):
        return self.capacity - self.pending

    def put(self, data, now):
        if len(data) > self.available:
            raise BufferError("link queue capacity exceeded")
        if not data:
            return
        ready = max(self.tail, now + max(0, self.latency + self.random.uniform(-self.jitter, self.jitter)))
        if self.rate:
            ready += len(data) / self.rate
        self.tail = ready
        self.queue.append([ready, memoryview(data)])
        self.pending += len(data)
        self.received += len(data)
        self.peak = max(self.peak, self.pending)

    def ready(self, now):
        return bool(self.queue and self.queue[0][0] <= now)

    def write(self, fd, now):
        if not self.ready(now):
            return
        try:
            count = os.write(fd, self.queue[0][1])
        except BlockingIOError:
            return
        if count == 0:
            raise BrokenPipeError("zero-length link write")
        self.pending -= count
        self.transferred += count
        self.writes += 1
        self.queue[0][1] = self.queue[0][1][count:]
        if not self.queue[0][1]:
            self.queue.popleft()

    def metrics(self):
        return {key: getattr(self, key) for key in
                ("received", "transferred", "writes", "pending", "peak", "capacity")}


def build_disk_shim(directory):
    source = Path(__file__).parent / "tests/fixtures/bridge_slow_disk.c"
    output = Path(directory) / "bridge_slow_disk.so"
    subprocess.run(["cc", "-O2", "-shared", "-fPIC", "-pthread", str(source), "-ldl", "-o", str(output)], check=True)
    return output


def burn(percent):
    # A separate process shares the command's one allowed CPU. Only its own
    # process group is affected; the harness never changes system CPU limits.
    period = 0.02
    while True:
        start = time.monotonic()
        while time.monotonic() - start < period * percent / 100:
            pass
        time.sleep(max(0, period - (time.monotonic() - start)))


@contextmanager
def child_environment(args, temporary):
    env = os.environ.copy()
    if args.disk_delay_ms:
        if not args.disk_root:
            raise ValueError("--disk-delay-ms requires --disk-root (an isolated test workspace)")
        root = str(Path(args.disk_root).resolve(strict=True))
        if root == "/":
            raise ValueError("refusing to delay the entire filesystem")
        env["VTCODE_LAB_DISK_ROOT"] = root
        env["VTCODE_LAB_DISK_DELAY_US"] = str(round(args.disk_delay_ms * 1000))
        shim = str(build_disk_shim(temporary))
        env["LD_PRELOAD"] = shim + (":" + env["LD_PRELOAD"] if env.get("LD_PRELOAD") else "")
    yield env


def stop_child(pid):
    # pid is returned by our forkpty, which creates a new session/process group.
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.killpg(pid, sig)
        except ProcessLookupError:
            pass
        until = time.monotonic() + 0.5
        while time.monotonic() < until:
            found, status, usage = os.wait4(pid, os.WNOHANG)
            if found:
                return os.waitstatus_to_exitcode(status), usage
            time.sleep(0.01)
    _, status, usage = os.wait4(pid, 0)
    return os.waitstatus_to_exitcode(status), usage


def run(args):
    if sys.platform != "linux":
        raise ValueError("run the PTY/CPU/disk fault wrapper on the Linux target; Windows can run its unit tests")
    import fcntl
    import pty
    import resource
    import termios
    import tty

    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        raise ValueError("supply -- command [arguments]")
    start = time.monotonic()
    wrapper_start = resource.getrusage(resource.RUSAGE_SELF)
    workload_usage = None
    links = [Link(args.latency_ms / 1000, args.jitter_ms / 1000,
                  args.bytes_per_second, args.max_buffer, args.seed + index) for index in range(2)]
    saved_tty = termios.tcgetattr(0) if os.isatty(0) else None
    blockers = {fd: os.get_blocking(fd) for fd in (0, 1)}
    previous_resize = signal.getsignal(signal.SIGWINCH)
    previous_term = signal.getsignal(signal.SIGTERM)
    previous_hup = signal.getsignal(signal.SIGHUP)
    cpu = None
    status = None
    pid = None
    master = None
    reason = "exit"
    resized = 0
    with tempfile.TemporaryDirectory(prefix="vtcode-bridge-lab-") as temporary:
        with child_environment(args, temporary) as env:
            try:
                def terminate(*_):
                    # A second hangup must not interrupt the cleanup below.
                    signal.signal(signal.SIGTERM, signal.SIG_IGN)
                    signal.signal(signal.SIGHUP, signal.SIG_IGN)
                    raise KeyboardInterrupt

                signal.signal(signal.SIGTERM, terminate)
                signal.signal(signal.SIGHUP, terminate)
                pid, master = pty.fork()
                if pid == 0:
                    try:
                        tty.setraw(0)
                        if args.single_core or args.cpu_load_percent:
                            os.sched_setaffinity(0, {min(os.sched_getaffinity(0))})
                        os.execvpe(command[0], command, env)
                    except BaseException as error:
                        os.write(2, f"bridge lab could not start command: {error}\n".encode())
                        os._exit(127)
                if args.cpu_load_percent:
                    cpu = subprocess.Popen([sys.executable, str(Path(__file__).resolve()), "--burn", str(args.cpu_load_percent)],
                                           stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                           start_new_session=True)
                    os.sched_setaffinity(cpu.pid, {min(os.sched_getaffinity(0))})

                def resize(*_):
                    nonlocal resized
                    if saved_tty is not None:
                        size = fcntl.ioctl(0, termios.TIOCGWINSZ, bytes(8))
                        fcntl.ioctl(master, termios.TIOCSWINSZ, size)
                        resized += 1

                signal.signal(signal.SIGWINCH, resize)
                resize()
                if saved_tty is not None:
                    tty.setraw(0)
                for fd in (0, 1, master):
                    os.set_blocking(fd, False)
                sources = {0: (links[0], master), master: (links[1], 1)}
                ended = set()
                while True:
                    now = time.monotonic()
                    elapsed = now - start
                    if master in ended and not links[1].pending:
                        found, result, usage = os.wait4(pid, os.WNOHANG)
                        if found:
                            status = os.waitstatus_to_exitcode(result)
                            workload_usage = usage
                            break
                    if args.disconnect_after_ms and elapsed * 1000 >= args.disconnect_after_ms:
                        reason = "disconnect"
                        break
                    if elapsed >= args.timeout:
                        reason = "timeout"
                        break
                    stalled = (args.stall_ms > 0 and args.stall_after_ms <= elapsed * 1000 < args.stall_after_ms + args.stall_ms)
                    reads = [source for source, (link, _) in sources.items() if source not in ended and link.available]
                    writes = [dest for link, dest in sources.values() if link.ready(now) and not stalled]
                    deadlines = [link.queue[0][0] for link, _ in sources.values() if link.queue and not link.ready(now)]
                    delay = max(0, min([0.05] + [deadline - now for deadline in deadlines]))
                    ready_read, ready_write, _ = select.select(reads, writes, [], delay)
                    for source in ready_read:
                        link, _ = sources[source]
                        try:
                            data = os.read(source, min(args.chunk_size, link.available))
                        except BlockingIOError:
                            continue
                        except OSError as error:
                            if source == master and error.errno == errno.EIO:
                                data = b""
                            else:
                                raise
                        if data:
                            link.put(data, time.monotonic())
                        else:
                            ended.add(source)
                    for link, dest in sources.values():
                        if dest in ready_write:
                            link.write(dest, time.monotonic())
                return status if status is not None else 124
            except (KeyboardInterrupt, BrokenPipeError):
                reason = "cancelled"
                return 130
            finally:
                signal.signal(signal.SIGTERM, signal.SIG_IGN)
                signal.signal(signal.SIGHUP, signal.SIG_IGN)
                if pid and status is None:
                    status, workload_usage = stop_child(pid)
                if cpu is not None:
                    cpu.terminate()
                    try:
                        cpu.wait(timeout=1)
                    except subprocess.TimeoutExpired:
                        cpu.kill()
                        cpu.wait()
                signal.signal(signal.SIGWINCH, previous_resize)
                if saved_tty is not None:
                    try:
                        termios.tcsetattr(0, termios.TCSANOW, saved_tty)
                    except termios.error as error:
                        if error.args[0] not in (errno.EIO, errno.ENOTTY, errno.EBADF):
                            raise
                for fd, blocking in blockers.items():
                    os.set_blocking(fd, blocking)
                if master is not None:
                    os.close(master)
                if args.metrics:
                    wrapper_end = resource.getrusage(resource.RUSAGE_SELF)
                    result = {"elapsed_seconds": time.monotonic() - start, "reason": reason,
                              "exit_status": status, "resizes": resized,
                              "input": links[0].metrics(), "output": links[1].metrics(),
                              "profile": {key: value for key, value in vars(args).items() if key != "command"},
                              "wrapper_cpu_seconds": (wrapper_end.ru_utime - wrapper_start.ru_utime
                                                      + wrapper_end.ru_stime - wrapper_start.ru_stime),
                              "workload": None if workload_usage is None else {
                                  "user_cpu_seconds": workload_usage.ru_utime,
                                  "system_cpu_seconds": workload_usage.ru_stime,
                                  "peak_rss_kib": workload_usage.ru_maxrss,
                                  "voluntary_context_switches": workload_usage.ru_nvcsw,
                                  "involuntary_context_switches": workload_usage.ru_nivcsw,
                                  "input_block_operations": workload_usage.ru_inblock,
                                  "output_block_operations": workload_usage.ru_oublock}}
                    Path(args.metrics).write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
                signal.signal(signal.SIGTERM, previous_term)
                signal.signal(signal.SIGHUP, previous_hup)


def parser():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--latency-ms", type=float, default=0, help="one-way delay per direction (RTT adds twice this)")
    p.add_argument("--jitter-ms", type=float, default=0)
    p.add_argument("--bytes-per-second", type=int, default=0, help="per direction; 0 is unlimited")
    p.add_argument("--chunk-size", type=int, default=4096, help="1 stresses every byte boundary")
    p.add_argument("--max-buffer", type=int, default=1024 * 1024, help="hard queue bound per direction")
    p.add_argument("--seed", type=int, default=1)
    p.add_argument("--stall-after-ms", type=float, default=0)
    p.add_argument("--stall-ms", type=float, default=0)
    p.add_argument("--disconnect-after-ms", type=float, default=0)
    p.add_argument("--single-core", action="store_true")
    p.add_argument("--cpu-load-percent", type=float, default=0)
    p.add_argument("--disk-delay-ms", type=float, default=0, help="delay each workspace file I/O call")
    p.add_argument("--disk-root", help="only this isolated directory and its descendants are delayed")
    p.add_argument("--metrics", help="write JSON counters and exact fault profile")
    p.add_argument("--timeout", type=float, default=120)
    p.add_argument("command", nargs=argparse.REMAINDER)
    return p


def main():
    if len(sys.argv) == 3 and sys.argv[1] == "--burn":
        return burn(float(sys.argv[2]))
    args = parser().parse_args()
    if any(getattr(args, name) < 0 for name in ("latency_ms", "jitter_ms", "bytes_per_second", "stall_after_ms",
           "stall_ms", "disconnect_after_ms", "disk_delay_ms")) or args.chunk_size < 1 or args.timeout <= 0:
        raise ValueError("delays/rates must be nonnegative; chunk size/timeout must be positive")
    if not 0 <= args.cpu_load_percent <= 100:
        raise ValueError("CPU load must be between 0 and 100 percent")
    return run(args)


if __name__ == "__main__":
    raise SystemExit(main())
