#!/usr/bin/env python3
"""Measure headless file latency while metadata runs on a slow Linux disk.

Run on the service machine, e.g. through SSH. Only a private temporary fixture
is delayed; no system settings, mounts or user files are changed. The existing
bridge_lab LD_PRELOAD shim supplies per-operation delay. Worker /proc state
confirms that the delayed stat has started before foreground requests are sent.

python3 scripts/bridge_metadata_bench.py /path/to/vtcode --expect-isolated
"""
from __future__ import annotations

import argparse
import base64
import json
import os
from pathlib import Path
import queue
import statistics
import subprocess
import tempfile
import threading
import time
import zlib

from bridge_lab import build_disk_shim


def encode(value):
    return base64.urlsafe_b64encode(value).rstrip(b"=")


def decode(value):
    return base64.urlsafe_b64decode(value + b"=" * (-len(value) % 4))


class Peer:
    def __init__(self, binary, root, env):
        self.child = subprocess.Popen(
            [str(binary), "--headless", str(root)], env=env,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        ready = self.child.stdout.readline()
        hello = json.loads(decode(ready.split(b"VTCR-READY:", 1)[1].split(b"\x07")[0].strip()))
        self.version, self.session = hello["version"], hello["session"]
        self.ident = 0
        self.replies = queue.Queue()
        self.completed = {}
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        try:
            pending = {}
            for line in self.child.stdout:
                record = line.split(b"vtcode-headless;", 1)[-1].split(b"\x07")[0].strip()
                parts = record.split(b":", 5)
                if len(parts) != 6 or parts[0] != f"VTCR{self.version}S".encode():
                    continue
                ident = int(parts[2], 16)
                data = pending.setdefault(ident, bytearray())
                data.extend(decode(parts[5]))
                if parts[4] == b"1":
                    del pending[ident]
                    plain = data[1:] if data[0] == 0 else zlib.decompress(data[1:])
                    self.replies.put((ident, time.monotonic(), json.loads(plain)["result"]))
        except BaseException as error:
            self.replies.put((-1, time.monotonic(), error))

    def send(self, command, notification=False):
        if not notification:
            self.ident += 1
        ident = 0 if notification else self.ident
        data = b"\0" + json.dumps({"id": ident, "command": command}, separators=(",", ":")).encode()
        for sequence, start in enumerate(range(0, len(data), 2048)):
            chunk = data[start:start + 2048]
            frame = (f"VTCR{self.version}C:{self.session:016x}:{ident:016x}:{sequence:x}:"
                     f"{int(start + len(chunk) == len(data))}:").encode()
            self.child.stdin.write(frame + encode(chunk) + b"\n")
        self.child.stdin.flush()
        return ident

    def receive(self, ident):
        deadline = time.monotonic() + 30
        while ident not in self.completed:
            found, when, value = self.replies.get(timeout=max(0, deadline - time.monotonic()))
            if found == -1:
                raise value
            self.completed[found] = (when, value)
        return self.completed[ident]

    def call(self, command):
        _, value = self.receive(self.send(command))
        if "Err" in value:
            raise RuntimeError(value)
        return value["Ok"]

    def wait_for_slow_stat(self):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            for task in Path(f"/proc/{self.child.pid}/task").iterdir():
                try:
                    name = (task / "comm").read_text().strip()
                    if name in ("headless-files", "headless-metada") and "nanosleep" in (task / "wchan").read_text():
                        return
                except FileNotFoundError:
                    pass
            time.sleep(.001)
        raise RuntimeError("The delayed metadata worker never started")

    def stop(self):
        try:
            self.call("Shutdown")
            assert self.child.wait(timeout=10) == 0
        finally:
            if self.child.poll() is None:
                self.child.kill()
                self.child.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=lambda value: Path(value).resolve(strict=True))
    parser.add_argument("--samples", type=int, default=5)
    parser.add_argument("--files", type=int, default=64)
    parser.add_argument("--delay-ms", type=int, default=20)
    parser.add_argument("--expect-isolated", action="store_true")
    args = parser.parse_args()
    if not (1 <= args.files <= 1024 and 1 <= args.samples <= 100 and 1 <= args.delay_ms <= 1000):
        parser.error("files: 1..1024, samples: 1..100, delay-ms: 1..1000")
    with tempfile.TemporaryDirectory(prefix="vtcode-metadata-bench-") as private:
        root = Path(private)
        slow = root / "slow"
        slow.mkdir()
        for number in range(args.files):
            (slow / f"{number}.txt").write_text("background metadata\n")
        (root / "edit.txt").write_text("a line\n" * 100)
        shim = build_disk_shim(root)
        env = dict(os.environ, LD_PRELOAD=str(shim), VTCODE_LAB_DISK_ROOT=str(slow),
                   VTCODE_LAB_DISK_DELAY_US=str(args.delay_ms * 1000))
        peer = Peer(args.binary, root, env)
        path = encode(b"edit.txt").decode()
        paths = [encode(f"slow/{number}.txt".encode()).decode() for number in range(args.files)]
        samples = []
        try:
            for sample in range(args.samples):
                opened = peer.call({"Open": {"path": path}})
                opened = opened["Text"]["Opened"] if "Text" in opened else opened["Opened"]
                revision = opened["revision"]
                if "transfer" in opened:
                    peer.call({"Text": {"Close": {"transfer": opened["transfer"]}}})
                check = peer.send({"Inspect": {"paths": paths}})
                peer.wait_for_slow_stat()
                start = time.monotonic()
                opened_id = peer.send({"Open": {"path": path}})
                save_start = time.monotonic()
                saved_id = peer.send({"Save": {"path": path, "base": revision,
                    "encoding": "UTF-8", "change": {"start": 0, "end": 0, "text": "x"}}})
                opened_at, opened = peer.receive(opened_id)
                opened = opened["Ok"]
                opened = opened["Text"]["Opened"] if "Text" in opened else opened["Opened"]
                if "transfer" in opened:
                    peer.call({"Text": {"Close": {"transfer": opened["transfer"]}}})
                saved_at, saved = peer.receive(saved_id)
                assert "Saved" in saved["Ok"], saved
                checked_at, checked = peer.receive(check)
                assert len(checked["Ok"]["FileStates"]) == args.files
                result = {"sample": sample, "open_ms": (opened_at - start) * 1000,
                          "save_ms": (saved_at - save_start) * 1000,
                          "open_and_save_finish_before_metadata": max(opened_at, saved_at) < checked_at}
                samples.append(result)
                print(json.dumps(result), flush=True)
                peer.call({"Close": {"path": path}})
            check = peer.send({"Inspect": {"paths": paths}})
            peer.wait_for_slow_stat()
            cancelled_at = time.monotonic()
            peer.send({"Cancel": {"request": check}}, notification=True)
            next_id = peer.send({"Inspect": {"paths": [path]}})
            next_at, next_value = peer.receive(next_id)
            assert next_value["Ok"]["FileStates"][0]["exists"]
            _, cancelled = peer.receive(check)
            summary = {"version": peer.version, "files": args.files, "delay_ms_per_stat": args.delay_ms,
                       "median_open_ms": statistics.median(row["open_ms"] for row in samples),
                       "median_save_ms": statistics.median(row["save_ms"] for row in samples),
                       "isolated": all(row["open_and_save_finish_before_metadata"] for row in samples),
                       "cancelled": "Err" in cancelled,
                       "next_check_after_cancel_ms": (next_at - cancelled_at) * 1000}
            print(json.dumps(summary), flush=True)
            if args.expect_isolated:
                assert summary["isolated"] and summary["cancelled"], summary
        finally:
            peer.stop()


if __name__ == "__main__":
    main()
