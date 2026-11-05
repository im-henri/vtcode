use super::*;
use magika::Input;

fn label(source: &str) -> Option<&'static str> {
    let cancel = CancellationToken::new();
    let sample = Snapshot::capture(&Rope::from_str(source))
        .sample(&cancel)
        .unwrap();
    classify(&sample, &cancel).map(|info| info.label)
}

#[test]
fn magika_recognizes_common_and_less_common_languages() {
    let mut failures = Vec::new();
    for (expected, source) in [
        (
            "python",
            "import sys\n\ndef greet(name):\n    print(name)\n",
        ),
        (
            "javascript",
            "const greet = (name) => { console.log(name); };\n",
        ),
        (
            "typescript",
            "export interface Item { name: string; }\nconst value: number = 1;\n",
        ),
        ("rust", "use std::io;\nfn main() { println!(\"hello\"); }\n"),
        (
            "go",
            "package main\nimport \"fmt\"\nfunc main() { fmt.Println(1) }\n",
        ),
        (
            "c",
            "#include <stdio.h>\nint main(void) { printf(\"hi\"); return 0; }\n",
        ),
        ("cpp", "#include <vector>\nstd::vector<int> values;\n"),
        (
            "cs",
            "using System;\nnamespace Example {\n    class Program {\n        static void Main(string[] args) {\n            Console.WriteLine(\"Hello world\");\n        }\n    }\n}\n",
        ),
        (
            "java",
            "package example;\nimport java.util.List;\npublic class Main {\n    public static void main(String[] args) {\n        System.out.println(\"Hello world\");\n    }\n}\n",
        ),
        (
            "shell",
            "#!/bin/bash\nif [ -f file ]; then\n  echo found\nfi\n",
        ),
        ("powershell", "param([string]$Name)\nWrite-Host $Name\n"),
        ("php", "<?php\necho \"hello\";\n"),
        (
            "ruby",
            "require 'json'\ndef greet(name)\n  puts name\nend\n",
        ),
        (
            "lua",
            "local value = 1\nfunction greet(name)\n print(name)\nend\n",
        ),
        (
            "html",
            "<!doctype html>\n<html><body><p>Hello</p></body></html>",
        ),
        (
            "xml",
            "<?xml version=\"1.0\"?><catalog><item>hello</item></catalog>",
        ),
        ("css", ".card { color: red; padding: 12px; }\n"),
        (
            "markdown",
            "# Heading\n\n- A point\n\n[link](https://example.com)\n",
        ),
        ("sql", "SELECT name FROM users WHERE enabled = 1;\n"),
        ("toml", "[package]\nname = \"example\"\nversion = \"1.0\"\n"),
        ("yaml", "service:\n  enabled: true\n  port: 8080\n"),
        ("json", "{\"name\": \"example\", \"items\": [1, 2, 3]}"),
        ("ini", "[server]\nhost = localhost\nport = 8080\n"),
        ("batch", "@echo off\nsetlocal\necho Hello\n"),
        (
            "dockerfile",
            "FROM alpine:3\nRUN echo hello\nCMD [\"sh\"]\n",
        ),
        (
            "cmake",
            "cmake_minimum_required(VERSION 3.20)\nproject(example)\n",
        ),
        (
            "clojure",
            "(ns example.core)\n(defn greet [name] (println (str \"Hello \" name)))\n(greet \"world\")\n",
        ),
        (
            "elixir",
            "defmodule Example do\n  def greet(name) do\n    IO.puts(\"Hello #{name}\")\n  end\nend\n",
        ),
        (
            "erlang",
            "-module(example).\n-export([greet/1]).\ngreet(Name) -> io:format(\"Hello ~s~n\", [Name]).\n",
        ),
        (
            "haskell",
            "module Main where\nimport Data.List\nmain :: IO ()\nmain = putStrLn $ intercalate \", \" [\"hello\", \"world\"]\n",
        ),
        (
            "julia",
            "using LinearAlgebra\nfunction greet(name::String)\n    println(\"Hello $name\")\nend\ngreet(\"world\")\n",
        ),
        (
            "ocaml",
            "let rec factorial n =\n  if n <= 1 then 1 else n * factorial (n - 1)\nlet () = Printf.printf \"%d\\n\" (factorial 5)\n",
        ),
        (
            "pascal",
            "program Hello;\nvar name: string;\nbegin\n  name := 'world';\n  writeln('Hello ', name);\nend.\n",
        ),
        (
            "perl",
            "use strict;\nuse warnings;\nmy $name = 'world';\nprint \"Hello $name\\n\";\n",
        ),
        (
            "r",
            "library(ggplot2)\nvalues <- c(1, 2, 3, 4)\nprint(mean(values))\n",
        ),
        (
            "scala",
            "object Main {\n  def main(args: Array[String]): Unit = {\n    println(\"Hello world\")\n  }\n}\n",
        ),
        (
            "swift",
            "import Foundation\nfunc greet(name: String) -> String {\n    return \"Hello \\(name)\"\n}\nprint(greet(name: \"world\"))\n",
        ),
    ] {
        let actual = label(source);
        if actual != Some(expected) {
            failures.push(format!("{expected}: {actual:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn ordinary_text_and_binary_are_not_assigned_a_programming_language() {
    for source in [
        "",
        "Hello world!",
        "Some ordinary prose about a class of functions.",
        "[2026-11-05] INFO build finished\n[2026-11-05] WARN retrying\n",
        "\0ELF payload",
        "☃ 界 👩‍💻\n",
    ] {
        assert_eq!(label(source), None, "{source}");
    }
}

#[test]
fn snapshots_preserve_exact_model_input_with_constant_retained_size() {
    let cancel = CancellationToken::new();
    for content in [
        String::new(),
        "short".into(),
        "x".repeat(4096),
        "x".repeat(4097),
        format!("a{}b", "☃界👩".repeat(100_000)),
    ] {
        let snapshot = Snapshot::capture(&Rope::from_str(&content));
        assert!(snapshot.retained_bytes() <= SAMPLE_BLOCK_BYTES * 2 + 6);
        let sample = snapshot.sample(&cancel).unwrap();
        let count = content.len().min(SAMPLE_BLOCK_BYTES);
        assert_eq!(sample.len, content.len() as u64);
        assert_eq!(sample.head, content.as_bytes()[..count]);
        assert_eq!(sample.tail, content.as_bytes()[content.len() - count..]);
        let mut read = vec![0; count];
        (&sample).read_at(&mut read, 0).unwrap();
        assert_eq!(read, sample.head);
        (&sample)
            .read_at(&mut read, (content.len() - count) as u64)
            .unwrap();
        assert_eq!(read, sample.tail);
        assert!(
            (&sample)
                .read_at(&mut [0; 1], content.len() as u64 + 1)
                .is_err()
        );
    }
    let snapshot = Snapshot::capture(&Rope::from_str("fn main() {}\n"));
    let sample = snapshot.sample(&cancel).unwrap();
    cancel.cancel();
    assert!(snapshot.sample(&cancel).is_none());
    assert!(classify(&sample, &cancel).is_none());
}

#[test]
fn bounded_input_agrees_with_the_upstream_whole_content_api() {
    let cancel = CancellationToken::new();
    let content = format!(
        "// ☃ Example\n{}fn main() {{ println!(\"hello\"); }}\n",
        "// comment\n".repeat(10_000)
    );
    let sample = Snapshot::capture(&Rope::from_str(&content))
        .sample(&cancel)
        .unwrap();
    let actual = classify(&sample, &cancel).map(|info| info.label);
    let mut model = MODEL.as_ref().unwrap().session().unwrap();
    let expected = model.identify_content(content.as_bytes()).unwrap();
    let info = expected.info();
    assert_eq!(
        actual,
        (info.is_text && info.label != "txt").then_some(info.label)
    );
}

#[test]
fn fingerprints_cover_both_ends_and_length_and_hints_are_bounded() {
    let fingerprint = |source: &str| {
        Snapshot::capture(&Rope::from_str(source))
            .sample(&CancellationToken::new())
            .unwrap()
            .fingerprint()
    };
    let source = format!("{}a{}", "x".repeat(5000), "y".repeat(5000));
    assert_eq!(
        fingerprint(&source),
        fingerprint(&source.replacen('a', "b", 1))
    );
    assert_ne!(fingerprint(&source), fingerprint(&format!("b{source}")));
    let mut changed = source.clone();
    changed.pop();
    changed.push('z');
    assert_ne!(fingerprint(&source), fingerprint(&changed));
    let sample = Snapshot::capture(&Rope::from_str(&"界".repeat(100_000)))
        .sample(&CancellationToken::new())
        .unwrap();
    assert!(sample.first_line().chars().count() <= 256);
}

#[test]
#[ignore = "manual cold/warm CPU benchmark"]
fn benchmark_content_detection() {
    use std::time::Instant;
    let cancel = CancellationToken::new();
    let rope = Rope::from_str(&"use std::io;\nfn main() { println!(\"hello\"); }\n".repeat(50_000));
    let start = Instant::now();
    let sample = Snapshot::capture(&rope).sample(&cancel).unwrap();
    assert_eq!(classify(&sample, &cancel).unwrap().label, "rust");
    eprintln!("cold detection: {:?}", start.elapsed());
    let start = Instant::now();
    for _ in 0..1000 {
        std::hint::black_box(Snapshot::capture(&rope));
    }
    eprintln!("UI snapshot capture: {:?}/request", start.elapsed() / 1000);
    let start = Instant::now();
    for _ in 0..100 {
        let sample = Snapshot::capture(&rope).sample(&cancel).unwrap();
        assert_eq!(classify(&sample, &cancel).unwrap().label, "rust");
    }
    eprintln!(
        "warm detection: {:?}/request; {}-byte document",
        start.elapsed() / 100,
        rope.len_bytes()
    );
}
