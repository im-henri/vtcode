#!/usr/bin/env python3
"""Measure current headless CPU by thread on Linux, without changing the process.

Run beside the headless process, including on an allocated compute node:
    python3 headless_cpu_probe.py PID --seconds 30 > headless-cpu.json

Only two /proc snapshots are taken. There is no tracing, polling loop, process
signal, SSH request, or permanent instrumentation. Percentages use one CPU core
as 100%, matching top's normal per-process scale. The process total excludes
child processes such as language servers and terminals.
"""
from __future__ import annotations

import argparse
from dataclasses import dataclass
import json
import math
import os
from pathlib import Path
import sys
import time


@dataclass(frozen=True)
class Counters:
    name: str
    start_ticks: int
    cpu_ticks: int


@dataclass(frozen=True)
class Sample:
    when: float
    process: Counters
    threads: dict[int, Counters]
    waits: dict[int, str]
    io: dict[str, int]


def parse_stat(text: str) -> Counters:
    # comm is parenthesized and may itself contain whitespace or parentheses.
    start, end = text.index('('), text.rindex(')')
    fields = text[end + 1:].split()
    return Counters(text[start + 1:end], int(fields[19]), int(fields[11]) + int(fields[12]))


def read_sample(pid: int, proc: Path = Path('/proc')) -> Sample:
    root = proc / str(pid)
    process = parse_stat((root / 'stat').read_text())
    when = time.monotonic()
    threads, waits = {}, {}
    for task in (root / 'task').iterdir():
        try:
            threads[int(task.name)] = parse_stat((task / 'stat').read_text())
        except FileNotFoundError:
            # A worker can finish while /proc is being read. The process total
            # still includes its CPU even when its thread counters disappear.
            continue
        try:
            waits[int(task.name)] = (task / 'wchan').read_text().strip()
        except OSError:
            waits[int(task.name)] = 'unavailable'
    io = {}
    try:
        for line in (root / 'io').read_text().splitlines():
            key, value = line.split(':', 1)
            io[key] = int(value)
    except OSError:
        pass  # Some cluster kernels restrict I/O counters independently.
    return Sample(when, process, threads, waits, io)


def report(pid: int, before: Sample, after: Sample, ticks_per_second: int) -> dict:
    if before.process.start_ticks != after.process.start_ticks:
        raise ProcessLookupError('The sampled PID was reused by another process; retry with the headless PID.')
    elapsed = after.when - before.when
    if elapsed <= 0 or ticks_per_second <= 0:
        raise ValueError('The sampling interval and CPU clock frequency must be positive.')

    def usage(old: Counters, new: Counters) -> dict:
        ticks = new.cpu_ticks - old.cpu_ticks
        if ticks < 0:
            raise ValueError('CPU counters moved backwards; the sample cannot be compared.')
        seconds = ticks / ticks_per_second
        return {'cpu_ms': round(seconds * 1000, 3),
                'cpu_percent_one_core': round(seconds / elapsed * 100, 6)}

    threads = []
    for tid, current in after.threads.items():
        previous = before.threads.get(tid)
        complete = previous is not None and previous.start_ticks == current.start_ticks
        threads.append({'tid': tid, 'name': current.name,
                        'wait': after.waits.get(tid, 'unavailable'),
                        'complete_interval': complete,
                        **(usage(previous, current) if complete else
                           {'cpu_ms': None, 'cpu_percent_one_core': None})})
    threads.sort(key=lambda entry: (-(entry['cpu_ms'] or 0), entry['tid']))
    return {'pid': pid, 'name': after.process.name, 'elapsed_seconds': round(elapsed, 6),
            'clock_ticks_per_second': ticks_per_second,
            'cpu_resolution_ms': 1000 / ticks_per_second,
            **usage(before.process, after.process),
            'threads': threads,
            'threads_ended': sorted(before.threads.keys() - after.threads.keys()),
            'io_delta': {key: value - before.io[key] for key, value in after.io.items()
                         if key in before.io and value >= before.io[key]},
            'notes': [
                'Current CPU over this interval, not an average since startup.',
                '100% means one CPU core. Child process CPU is excluded.',
                'Zero means below the reported CPU counter resolution.',
                'The process total includes threads that ended during the interval.',
                'Thread names are truncated by Linux; wait locations are end-of-sample snapshots.',
                'Threads created during the interval have null CPU values; use the process total.',
                'I/O counters include protocol pipes; they are not a count of disk operations.',
            ]}


def positive_seconds(value: str) -> float:
    number = float(value)
    if not math.isfinite(number) or number <= 0:
        raise argparse.ArgumentTypeError('seconds must be a finite positive number')
    return number


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('pid', type=int, help='PID of vtcode --headless on this machine')
    parser.add_argument('--seconds', type=positive_seconds, default=30.0, help='measurement interval (default: 30)')
    args = parser.parse_args()
    if not sys.platform.startswith('linux'):
        parser.error('run this probe on the Linux machine hosting the headless process')
    if args.pid <= 0:
        parser.error('PID must be positive')
    try:
        before = read_sample(args.pid)
        time.sleep(args.seconds)
        after = read_sample(args.pid)
        result = report(args.pid, before, after, os.sysconf('SC_CLK_TCK'))
    except PermissionError as error:
        print(f'Cannot read process counters: {error}. Run as the user owning the headless process.', file=sys.stderr)
        return 1
    except FileNotFoundError:
        print('The process exited, or PID is not running on this machine. No CPU measurement was produced.', file=sys.stderr)
        return 1
    except (OSError, ValueError, IndexError) as error:
        print(f'Cannot measure headless CPU: {error}', file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
