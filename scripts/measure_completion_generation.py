"""Measure requested heap for the pure generation module, outside the editor.

Build fastrelease dependencies and run the development replay first. The Rust
allocator counts requested bytes, not allocator overhead, stacks, or process RSS.
Inputs and reporting storage are allocated before each measured call.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess


HARNESS = r'''
#![allow(dead_code)]
#[path=MODULE_PATH] mod patterns;
use std::alloc::{GlobalAlloc,Layout,System};
use std::sync::atomic::{AtomicUsize,Ordering::Relaxed};
static LIVE:AtomicUsize=AtomicUsize::new(0);
static PEAK:AtomicUsize=AtomicUsize::new(0);
fn added(n:usize){let next=LIVE.fetch_add(n,Relaxed)+n;PEAK.fetch_max(next,Relaxed);}
struct Allocator;
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self,l:Layout)->*mut u8 {let p=unsafe{System.alloc(l)};if !p.is_null(){added(l.size())}p}
    unsafe fn alloc_zeroed(&self,l:Layout)->*mut u8 {let p=unsafe{System.alloc_zeroed(l)};if !p.is_null(){added(l.size())}p}
    unsafe fn dealloc(&self,p:*mut u8,l:Layout){unsafe{System.dealloc(p,l)};LIVE.fetch_sub(l.size(),Relaxed);}
    unsafe fn realloc(&self,p:*mut u8,l:Layout,n:usize)->*mut u8 {
        let q=unsafe{System.realloc(p,l,n)};
        if !q.is_null(){if n>=l.size(){added(n-l.size())}else{LIVE.fetch_sub(l.size()-n,Relaxed);}}q
    }
}
#[global_allocator] static ALLOCATOR:Allocator=Allocator;
fn take<'a>(data:&mut &'a [u8])->&'a str {
    let n=u32::from_le_bytes(data[..4].try_into().unwrap()) as usize;
    let value=std::str::from_utf8(&data[4..4+n]).unwrap();*data=&data[4+n..];value
}
fn main(){
    let input=std::fs::read(std::env::args_os().nth(1).unwrap()).unwrap();
    let mut data=input.as_slice();let mut cases=Vec::new();
    while !data.is_empty(){cases.push((take(&mut data),take(&mut data),take(&mut data),take(&mut data),take(&mut data)=="true"));}
    let mut results=Vec::with_capacity(cases.len());
    for (label,left,right,prefix,start) in cases {
        let base=LIVE.load(Relaxed);PEAK.store(base,Relaxed);
        let result=patterns::predict(left,right,prefix,start,&||false).unwrap();
        let peak=PEAK.load(Relaxed).saturating_sub(base);
        let result_bytes=LIVE.load(Relaxed).saturating_sub(base);
        let row=(label,peak,result_bytes,result.candidates().count(),result.work,result.limited);
        drop(result);assert_eq!(LIVE.load(Relaxed),base,"retained allocation in {label}");
        results.push(row);
    }
    for (label,peak,result_bytes,candidates,work,limited) in results {
        println!("{label}\t{peak}\t{result_bytes}\t{candidates}\t{work}\t{limited}");
    }
}
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--replay", type=Path, required=True)
    parser.add_argument("--source", type=Path, default=Path("src/completion_patterns.rs"))
    parser.add_argument("--output", type=Path, default=Path("target/completion-generation-heap"))
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    replay = json.loads(args.replay.read_text(encoding="utf-8"))
    cases = []
    for i, row in enumerate(replay["rows"]):
        visible, prefix = row["visible"], row["prefix"]
        if "caret" in row:
            caret = row["caret"]
        elif row["group"] == "literal-context-conflict":
            caret = visible.rindex(" = incompatible")
        else:
            assert visible.endswith(prefix)
            caret = len(visible)
        left = visible[:caret - len(prefix)]
        complete = len(left) <= 1024
        left = left[-1024:]
        cases.append((f'{i}:{row["group"]}', left, visible[caret:][:128], prefix, str(complete).lower()))
    columns = "x1_2_3_4 " * 31
    stress = "".join(f"{columns}root{n}_{n+1}_{n+2}_{n+3}\n" for n in range(1, 9)) + columns
    cases.append(("work-cap-stress", stress, "", "ro", "true"))
    cases.append(("input-cap-stress", "x" * 4097, "", "ro", "true"))
    data = bytearray()
    for row in cases:
        for text in row:
            value = text.encode("utf-8")
            data += struct.pack("<I", len(value)) + value
    inputs = args.output / "inputs.bin"
    inputs.write_bytes(data)
    harness = args.output / "heap.rs"
    harness.write_text(HARNESS.replace("MODULE_PATH", json.dumps(str(args.source.resolve()))), encoding="utf-8")
    executable = args.output / "heap.exe"
    subprocess.run(["rustc", "--edition=2024", "-C", "opt-level=3", str(harness), "-o", str(executable)], check=True)
    result = subprocess.run([str(executable.resolve()), str(inputs.resolve())], check=True, capture_output=True, text=True)
    rows = []
    for line in result.stdout.splitlines():
        label, peak, retained_result, candidates, work, limited = line.split("\t")
        rows.append(dict(label=label, peakRequestedHeapBytes=int(peak), resultHeapBytes=int(retained_result),
                         candidates=int(candidates), work=int(work), limited=limited == "true", retainedAfterDropBytes=0))
    report = dict(moduleSha256=hashlib.sha256(args.source.read_bytes()).hexdigest(),
                  replaySha256=hashlib.sha256(args.replay.read_bytes()).hexdigest(),
                  measurement="requested heap; excludes inputs, reporting, stacks, allocator overhead and RSS",
                  maxReplayPeakRequestedHeapBytes=max(row["peakRequestedHeapBytes"] for row in rows[:-2]),
                  maxStressPeakRequestedHeapBytes=max(row["peakRequestedHeapBytes"] for row in rows[-2:]), rows=rows)
    output = args.output / "heap.json"
    output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(output)


if __name__ == "__main__":
    main()
