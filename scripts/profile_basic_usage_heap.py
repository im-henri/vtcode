"""Requested-allocation probes for content analysis and paste preparation.

Compile the actual production cache modules from each supplied source directory.
The paste control reproduces the previous normalize/reindent/String-to-Rope
pipeline. Inputs, editor state, stacks and allocator overhead are excluded.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess


ALLOCATOR = r'''
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
'''

CACHE = r'''
#![allow(dead_code)]
ALLOCATOR
#[path=CHANGES] mod rope_changes;
#[path=BLOCKS] mod rope_line_blocks;
mod background {
    #[derive(Clone,Copy)] pub struct StickyScopeLine {pub line:usize,pub parent:Option<usize>}
}
mod worker {
    #[path=ANALYSIS] pub mod analysis;
    // Same ASCII marker semantics; this cold-pass helper allocates no storage.
    fn is_marker_word_char(b:u8)->bool {b.is_ascii_alphanumeric() || b==b'_'}
    fn count_diagnostic_markers(text:&str)->(usize,usize) {
        let mut result=(0,0);
        for token in text.split(|ch:char| !ch.is_ascii_alphanumeric() && ch!='_') {
            match token {"BUG"|"FIXME"=>result.0+=1,"TODO"|"HACK"=>result.1+=1,_=>{}}
        }
        result
    }
}
fn main(){
    let mut reports=Vec::with_capacity(3);
    for count in [2000usize,40000,0] {
        let sources:Vec<_>=if count==0 {
            let root=std::path::PathBuf::from(std::env::var_os("VTCODE_BASIC_CORPUS").unwrap());
            ["animation_config.rs","animation_inline_scope_tests.rs","animation_language_scope_tests.rs",
                "animation_multicursor_tests.rs","animation_pacing.rs","animation_preprocessor_scope_tests.rs",
                "animation_scope.rs","animation_scope_blame_tests.rs","animation_scope_checkpoints.rs",
                "animation_scope_edit_tests.rs","animation_scope_edits.rs","animation_scope_indentation.rs",
                "animation_scope_preprocessor.rs"].iter().map(|name|ropey::Rope::from_str(
                    &std::fs::read_to_string(root.join(name)).unwrap())).collect()
        }else{(1..=3).map(|id|ropey::Rope::from_str(
            &format!("function_{id} {{\n    let value = compute(input); // TODO\n}}\n").repeat(count/3))).collect()};
        let base=LIVE.load(Relaxed);PEAK.store(base,Relaxed);
        let mut cache=worker::analysis::Cache::default();
        for (id,source) in sources.iter().enumerate(){drop(cache.analyze(id as u64,source,std::time::Instant::now()+std::time::Duration::from_secs(30),&||false).unwrap());}
        let retained=LIVE.load(Relaxed)-base;let cold_peak=PEAK.load(Relaxed)-base;
        let warm_base=LIVE.load(Relaxed);PEAK.store(warm_base,Relaxed);
        let id=sources.len()-1;
        let result=cache.analyze(id as u64,&sources[id],std::time::Instant::now()+std::time::Duration::from_secs(30),&||false).unwrap();
        let peak=PEAK.load(Relaxed)-warm_base;let output=LIVE.load(Relaxed)-warm_base;
        drop(result);drop(cache);assert_eq!(LIVE.load(Relaxed),base);
        reports.push((count,sources.len(),retained,cold_peak,peak,output));
    }
    for (count,documents,retained,cold_peak,peak,output) in reports {println!("CACHE\t{count}\t{documents}\t{retained}\t{cold_peak}\t{peak}\t{output}");}
}
'''

PASTE = r'''
#![allow(dead_code)]
ALLOCATOR
#[path=PREPARE] mod prepared;
NORMALIZE
fn old_prepare(text:&str,destination:Option<&str>)->ropey::Rope {
    let text=normalize_paste_line_endings(text);
    let Some(destination)=destination else {return ropey::Rope::from_str(&text)};
    let mut common:Option<&str>=None;
    for line in text.split('\n').filter(|line|!line.trim().is_empty()) {
        let indent=&line[..line.len()-line.trim_start_matches([' ','\t']).len()];
        common=Some(match common {None=>indent,Some(previous)=>{
            let equal=previous.bytes().zip(indent.bytes()).take_while(|(a,b)|a==b).count();&previous[..equal]
        }});
        if common==Some(""){break;}
    }
    if common.is_none() || (common==Some("") && destination.is_empty()){return ropey::Rope::from_str(&text)}
    let strip=common.unwrap().len();let mut result=String::with_capacity(text.len());
    for (index,line) in text.split('\n').enumerate(){
        if index>0{result.push('\n');}
        if line.trim().is_empty(){result.push_str(line)}else{
            if index>0{result.push_str(destination)}result.push_str(&line[strip..]);
        }
    }
    ropey::Rope::from_str(&result)
}
fn main(){
    let candidate=std::env::args().nth(1).unwrap()=="after";
    let mut rows=Vec::with_capacity(6);
    for size in [16*1024usize,2*1024*1024] {
        for (case,eol,indent) in [("lf-indented","\n",Some("    ")),("crlf-indented","\r\n",Some("    ")),("crlf-plain","\r\n",None)] {
            let line=format!("        let café = compute(界, value);{eol}");
            let payload=line.repeat(size.div_ceil(line.len()));
            let expected=old_prepare(&payload,indent);
            let base=LIVE.load(Relaxed);PEAK.store(base,Relaxed);
            let result=if candidate {prepared::prepare(&payload,indent,false).unwrap()}else{old_prepare(&payload,indent)};
            let peak=PEAK.load(Relaxed)-base;let output=LIVE.load(Relaxed)-base;
            assert_eq!(result,expected);drop(result);assert_eq!(LIVE.load(Relaxed),base);
            rows.push((payload.len(),case,peak,output));
        }
    }
    for (bytes,case,peak,output) in rows {println!("PASTE\t{bytes}\t{case}\t{peak}\t{output}");}
}
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline-source", type=Path, required=True)
    parser.add_argument("--candidate-source", type=Path, default=Path("src"))
    parser.add_argument("--output", type=Path, default=Path("target/basic-usage-heap"))
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    deps = Path("target/fastrelease/deps").resolve()
    externs = []
    for name in ["ropey", "memchr"]:
        library = sorted(deps.glob(f"lib{name}-*.rlib"))[0]
        externs.extend(["--extern", f"{name}={library}"])
    selection = (args.baseline_source / "editor_selection.rs").read_text(encoding="utf-8")
    start = selection.index("fn normalize_paste_line_endings(")
    normalize = selection[start:selection.index("\n}\n", start) + 2]
    report = {"measurement":"requested heap; excludes fixture inputs, editor state, stacks, allocator overhead and RSS", "runs":[]}
    for side, source in [("before", args.baseline_source), ("after", args.candidate_source)]:
        for kind, template in [("cache", CACHE), ("paste", PASTE)]:
            replacements = {"ALLOCATOR":ALLOCATOR, "NORMALIZE":normalize,
                            "CHANGES":json.dumps(str((source / "rope_changes.rs").resolve())),
                            "BLOCKS":json.dumps(str((source / "rope_line_blocks.rs").resolve())),
                            "ANALYSIS":json.dumps(str((source / "editor_content_analysis.rs").resolve())),
                            "PREPARE":json.dumps(str((args.candidate_source / "editor_paste_text.rs").resolve()))}
            for marker, value in replacements.items():
                template = template.replace(marker, value)
            harness = args.output / f"{side}-{kind}.rs"
            harness.write_text(template, encoding="utf-8")
            executable = harness.with_suffix(".exe")
            subprocess.run(["rustc", "--edition=2024", "-C", "opt-level=3", str(harness), "-L", f"dependency={deps}", *externs, "-o", str(executable)],check=True)
            environment = os.environ.copy()
            environment["VTCODE_BASIC_CORPUS"] = str(args.baseline_source.resolve())
            output = subprocess.run([str(executable.resolve()), side],check=True,capture_output=True,text=True,env=environment).stdout
            rows = []
            for line in output.splitlines():
                values=line.split("\t")
                if kind == "cache":
                    _,lines,documents,retained,cold_peak,peak,result=values
                    rows.append(dict(workload="reported-13" if lines == "0" else "repeated-scopes",linesPerDocument=int(lines) or None,documents=int(documents),cacheLiveRequestedBytes=int(retained),coldPeakRequestedBytes=int(cold_peak),warmQueryPeakAdditionalBytes=int(peak),warmResultAdditionalBytes=int(result)))
                else:
                    _,size,case,peak,result=values
                    rows.append(dict(inputBytes=int(size),case=case,peakRequestedBytes=int(peak),resultRequestedBytes=int(result)))
            report["runs"].append(dict(side=side,kind=kind,rows=rows))
    paths=[args.baseline_source / "editor_content_analysis.rs", args.baseline_source / "editor_selection.rs", args.candidate_source / "editor_content_analysis.rs", args.candidate_source / "editor_paste_text.rs"]
    report["sourceSha256"]={str(path):hashlib.sha256(path.read_bytes()).hexdigest() for path in paths}
    path=args.output / "heap.json"
    path.write_text(json.dumps(report,indent=2)+"\n",encoding="utf-8")
    print(path)


if __name__ == "__main__":
    main()
