//! Replay benchmark for GPTComplete inline prediction.
//!
//! Each source file is replayed as if it were being typed: at sampled lines
//! the remainder of the line is hidden and the predictor is asked for it.
//! `append` hides everything below the caret as well; `insert` keeps the
//! lines below visible, as when editing existing code. `comment` cases are
//! comment lines, which only the neural model continues.
//!
//! Usage: vtcomplete_inline_bench [--step N] [--refs N] [--first F] [--next F]
//!        [--block F] [--single-line] [--show N] PATH...
//!
//! `--try FILE` instead predicts at every `<|>` mark in a file and prints
//! the text with the evidence behind each token.
#[allow(dead_code)]
#[path = "../src/vtcomplete_inline.rs"]
mod inline;
#[path = "../src/vtcomplete_inline_retrieval.rs"]
mod retrieval;

use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Default, Clone)]
struct Tally {
    cases: usize,
    shown: usize,
    correct: usize,
    full: usize,
    saved: usize,
    wrong_chars: usize,
    truth_chars: usize,
    multi_shown: usize,
    multi_correct: usize,
}

/// First token, typed prefix, unused, unused, later line, estimate decile.
type Bucket = (bool, u32, usize, usize, bool, usize);

/// Set by `--classic`: replay only the kinds of file earlier versions of
/// this benchmark replayed, to compare with their results.
static CLASSIC: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
const CLASSIC_KINDS: &[&str] = &[
    "py", "rs", "c", "h", "cpp", "cc", "cxx", "hpp", "hh", "js", "jsx", "mjs", "cjs", "ts", "tsx", "go",
    "java", "kt", "kts", "cs", "rb", "php", "swift", "ex", "exs", "scala", "lua", "sh", "bash", "zsh", "dart",
];

fn collect(path: &Path, files: &mut Vec<PathBuf>, exclude: &[String]) {
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("");
    if exclude.iter().any(|entry| match entry.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => name == entry,
    }) {
        return;
    }
    if path.is_dir() {
        if [".git", "node_modules", "vendor", "third_party", "dist"].contains(&name) {
            return;
        }
        let mut entries: Vec<_> = std::fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .collect();
        entries.sort();
        for entry in entries {
            collect(&entry, files, exclude);
        }
    } else if !any_language(path).is_empty()
        && (!CLASSIC.load(std::sync::atomic::Ordering::Relaxed)
            || CLASSIC_KINDS.contains(&path.extension().and_then(|e| e.to_str()).unwrap_or("")))
        && !name.contains(".min.")
        && !name.contains("-lock.")
        && !name.ends_with(".lock")
    {
        files.push(path.to_owned());
    }
}

/// Language of a file the model is trained on, by the editor's name for it.
fn language(path: &Path) -> &'static str {
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("");
    match name.to_ascii_lowercase().as_str() {
        "makefile" | "gnumakefile" => return "Makefile",
        "dockerfile" => return "Dockerfile",
        "cmakelists.txt" => return "CMake",
        _ => {}
    }
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "py" => "Python",
        "rs" => "Rust",
        "c" | "h" => "C",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => "C++",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "ts" | "tsx" => "TypeScript",
        "go" => "Go",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "cs" => "C#",
        "rb" => "Ruby",
        "php" => "PHP",
        "swift" => "Swift",
        "ex" | "exs" => "Elixir",
        "scala" => "Scala",
        "lua" => "Lua",
        "sh" | "bash" | "zsh" => "Shell Script",
        "dart" => "Dart",
        "md" | "markdown" | "rst" | "adoc" => "Markdown",
        "html" | "htm" | "xml" | "vue" | "svelte" => "HTML",
        "css" | "scss" | "less" => "CSS",
        "sql" => "SQL",
        "json" | "yaml" | "yml" | "toml" | "ini" => "YAML",
        // Trained on, but without a number of their own: the model has to
        // tell what they are from the text, as with a language new to it.
        "m" | "mm" => "Objective-C",
        "hs" => "Haskell",
        "erl" | "hrl" => "Erlang",
        "r" | "R" => "R",
        "fs" | "fsx" => "F#",
        "ps1" | "psm1" => "PowerShell",
        "tf" => "Terraform",
        "vim" => "Vim Script",
        "cmake" => "CMake",
        "mk" => "Makefile",
        "gradle" | "groovy" => "Groovy",
        "proto" => "Protobuf",
        _ => "",
    }
}

/// Languages kept out of training altogether, to measure the model on a
/// language it has never seen.
fn unseen_language(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "zig" => "Zig",
        "jl" => "Julia",
        "nim" | "nims" => "Nim",
        "ml" | "mli" => "OCaml",
        "cr" => "Crystal",
        "gleam" => "Gleam",
        "clj" | "cljs" | "cljc" => "Clojure",
        _ => "",
    }
}

fn any_language(path: &Path) -> &'static str {
    match language(path) {
        "" => unseen_language(path),
        name => name,
    }
}

/// The most files of one language that training takes.
const EXPORT_CAP: usize = 30_000;

/// Write the subword vocabulary and token streams the neural model is
/// trained on. Every twentieth file is held out for validation. No language
/// contributes more than `CAP` files, so that one large source does not drown
/// the others. Files in the unseen languages go to a stream of their own,
/// which the trainer only measures on.
///
/// A stream is one record of four bytes per token: the token, the file's
/// language, and a byte that is one in files written as a made-up language.
/// Every file starts with the document token. Some training files are written
/// a second time with their common words replaced by made-up ones and no
/// language: a language new to the model, to be picked up from the text.
fn export_training(directory: &Path, all: &[(PathBuf, String)]) {
    use inline::subwords::{self, Cache, Subwords};
    use rustc_hash::{FxHashMap, FxHashSet};
    const CAP: usize = EXPORT_CAP;
    const TOKENS: usize = 16_384;
    const COMMON: usize = 600;
    const MADE_UP: u64 = 12; // In a hundred training files.
    std::fs::create_dir_all(directory).unwrap();
    let mut by_language: std::collections::BTreeMap<&str, Vec<usize>> = Default::default();
    for (index, (path, text)) in all.iter().enumerate() {
        // Generated and minified files teach nothing about writing.
        let longest = text.lines().map(str::len).max().unwrap_or(0);
        if longest > 2000 {
            continue;
        }
        by_language.entry(any_language(path)).or_default().push(index);
    }
    let mut chosen = Vec::new();
    for (name, files) in &by_language {
        let stride = files.len().div_ceil(CAP).max(1);
        let kept: Vec<usize> = files.iter().copied().step_by(stride).collect();
        println!("{name}: {} of {} files", kept.len(), files.len());
        chosen.extend(kept);
    }
    chosen.sort_unstable();
    type Files<'a> = Vec<&'a (PathBuf, String)>;
    let (texts, unseen): (Files, Files) = chosen
        .iter()
        .map(|&index| &all[index])
        .partition(|(path, _)| !language(path).is_empty());
    let threads = std::thread::available_parallelism().map_or(4, |count| count.get()).min(16);
    let share = texts.len().div_ceil(threads).max(1);

    // How often each piece occurs in the training files.
    let started = Instant::now();
    let counted: Vec<FxHashMap<Box<[u8]>, u64>> = std::thread::scope(|scope| {
        let workers: Vec<_> = texts
            .chunks(share)
            .enumerate()
            .map(|(part, files)| {
                scope.spawn(move || {
                    let mut counts: FxHashMap<Box<[u8]>, u64> = FxHashMap::default();
                    for (offset, (_, text)) in files.iter().enumerate() {
                        if (part * share + offset) % 20 == 7 {
                            continue;
                        }
                        for (start, end) in subwords::pieces(text) {
                            let piece = &text.as_bytes()[start..end];
                            match counts.get_mut(piece) {
                                Some(count) => *count += 1,
                                None => {
                                    counts.insert(piece.into(), 1);
                                }
                            }
                        }
                    }
                    counts
                })
            })
            .collect();
        workers.into_iter().map(|worker| worker.join().unwrap()).collect()
    });
    let mut counts: FxHashMap<Box<[u8]>, u64> = FxHashMap::default();
    for part in counted {
        for (piece, count) in part {
            *counts.entry(piece).or_default() += count;
        }
    }
    let total = counts.len();
    // Pieces met once or twice teach no merge worth having.
    counts.retain(|_, count| *count >= 3);
    println!("{total} different pieces, {} kept, counted in {:.0?}", counts.len(), started.elapsed());
    // The commonest words, which a made-up language replaces.
    let mut words: Vec<(&[u8], u64)> = Vec::new();
    let mut merged_words: FxHashMap<&[u8], u64> = FxHashMap::default();
    for (piece, &count) in &counts {
        let word = piece.strip_prefix(b" ").unwrap_or(piece);
        if word.len() >= 2 && word.iter().all(|byte| byte.is_ascii_alphabetic()) {
            *merged_words.entry(word).or_default() += count;
        }
    }
    words.extend(merged_words);
    words.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let common: FxHashSet<Vec<u8>> = words.iter().take(COMMON).map(|(word, _)| word.to_vec()).collect();

    let started = Instant::now();
    let merges = subwords::learn(&counts, TOKENS - 256 - 1);
    drop(counts);
    let subwords = Subwords::new(merges).unwrap();
    println!("{} tokens learned in {:.0?}", subwords.len(), started.elapsed());
    std::fs::write(directory.join("subwords.bin"), subwords.encode_merges()).unwrap();
    let document = subwords.document();

    // Write the streams; each thread writes its share of the files, in order.
    let encode_all = |files: &[&(PathBuf, String)], stream: &str, language_of: &(dyn Fn(&Path) -> u8 + Sync), made_up: bool| {
        let share = files.len().div_ceil(threads).max(1);
        let parts: Vec<(Vec<u8>, Vec<u8>)> = std::thread::scope(|scope| {
            let workers: Vec<_> = files
                .chunks(share)
                .enumerate()
                .map(|(part, files)| {
                    let (subwords, common) = (&subwords, &common);
                    scope.spawn(move || {
                        let mut cache = Cache::default();
                        let (mut train, mut valid) = (Vec::new(), Vec::new());
                        let (mut tokens, mut starts) = (Vec::new(), Vec::new());
                        let mut write = |out: &mut Vec<u8>, text: &str, language: u8, invented: bool, cache: &mut Cache| {
                            tokens.clear();
                            starts.clear();
                            tokens.push(document);
                            subwords.encode(text, cache, &mut tokens, &mut starts);
                            for &token in &tokens {
                                out.extend(token.to_le_bytes());
                                out.push(language);
                                out.push(u8::from(invented));
                            }
                        };
                        for (offset, (path, text)) in files.iter().enumerate() {
                            let index = part * share + offset;
                            let held_out = made_up && index % 20 == 7;
                            let out = if held_out { &mut valid } else { &mut train };
                            write(out, text, language_of(path), false, &mut cache);
                            if made_up && !held_out && hash(&path.to_string_lossy()) % 100 < MADE_UP {
                                let invented = invent(text, common, hash(&path.to_string_lossy()));
                                // Its own spellings: the made-up words are new.
                                let mut fresh = Cache::default();
                                write(&mut train, &invented, 0, true, &mut fresh);
                            }
                        }
                        (train, valid)
                    })
                })
                .collect();
            workers.into_iter().map(|worker| worker.join().unwrap()).collect()
        });
        let (mut train, mut valid) = (Vec::new(), Vec::new());
        for (a, b) in parts {
            train.extend(a);
            valid.extend(b);
        }
        std::fs::write(directory.join(format!("{stream}.tok")), &train).unwrap();
        if made_up {
            std::fs::write(directory.join("valid.tok"), &valid).unwrap();
        }
        (train.len() / 4, valid.len() / 4)
    };
    let started = Instant::now();
    let (train, valid) = encode_all(&texts, "train", &|path| inline::language_id(language(path)), true);
    let (other, _) = encode_all(&unseen, "unseen", &|_| 0, false);
    println!(
        "{} files, {} tokens, {train} training, {valid} validation and {other} unseen-language tokens, written in {:.0?}",
        texts.len(),
        subwords.len(),
        started.elapsed()
    );
}

/// `text` with most of its common words replaced by made-up ones, the same
/// one for every use of a word: the file as written in a new language.
fn invent(text: &str, common: &rustc_hash::FxHashSet<Vec<u8>>, seed: u64) -> String {
    let mut chosen: rustc_hash::FxHashMap<&str, String> = Default::default();
    let mut out = String::with_capacity(text.len());
    for (start, end) in inline::subwords::pieces(text) {
        let piece = &text[start..end];
        let word = piece.strip_prefix(' ').unwrap_or(piece);
        if !common.contains(word.as_bytes()) {
            out.push_str(piece);
            continue;
        }
        let stand_in = chosen.entry(word).or_insert_with(|| {
            let mut state = seed ^ hash(word);
            let mut next = || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state
            };
            // Three words in ten stay as they are: a new language usually
            // shares some words with the known ones.
            if next() % 10 < 3 {
                return word.to_owned();
            }
            let length = 2 + (next() % 6) as usize;
            let (consonants, vowels) = (b"bcdfghjklmnprstvwxz", b"aeiouy");
            let mut made: String = (0..length)
                .map(|at| {
                    let letters: &[u8] = if at % 2 == 0 { consonants } else { vowels };
                    letters[(next() % letters.len() as u64) as usize] as char
                })
                .collect();
            if word.starts_with(|ch: char| ch.is_ascii_uppercase()) {
                made[..1].make_ascii_uppercase();
            }
            made
        });
        if piece.starts_with(' ') {
            out.push(' ');
        }
        out.push_str(stand_in);
    }
    out
}

/// Predict at every `<|>` mark of a file, as if the caret stood there with
/// the other marks absent and the rest of its line not yet written, and
/// print what would be shown beside what the line really says.
fn try_marks(path: &Path, network: Option<Option<&'static inline::model::Network>>, options: inline::Options) {
    const MARK: &str = "<|>";
    let marked = std::fs::read_to_string(path).expect("readable file").replace("\r\n", "\n");
    let name = any_language(path);
    let pieces: Vec<&str> = marked.split(MARK).collect();
    for mark in 1..pieces.len() {
        let before = pieces[..mark].concat();
        let after = pieces[mark..].concat();
        let (rest, below) = after.split_once('\n').unwrap_or((&after, ""));
        let mut engine = inline::Engine::new();
        if let Some(network) = network {
            engine.set_network(network);
        }
        let caret = inline::Caret {
            key: 1,
            before: &before,
            rest: "",
            below,
            syntax: inline::Syntax::for_language(name),
            language: inline::language_id(name),
        };
        // The first request reads the whole context; the second is what
        // typing one more character costs.
        let started = Instant::now();
        let prediction = engine.predict(caret, options, &|| false);
        let cold = started.elapsed();
        let started = Instant::now();
        let _ = engine.predict(caret, options, &|| false);
        let warm = started.elapsed();
        let line = before.rsplit('\n').next().unwrap_or("");
        println!("{mark:>2} {line:?}
   is {rest:?}");
        match prediction {
            Some(prediction) => {
                if let Some(retype) = &prediction.retype {
                    println!("   respells the typed start as {retype:?}");
                }
                println!("   -> {:?}   ({:.0} ms first, {:.1} ms again)", prediction.text, cold.as_secs_f64() * 1e3, warm.as_secs_f64() * 1e3);
                let mut from = 0;
                for step in &prediction.trace {
                    let end = step.end.min(prediction.text.len());
                    let piece = prediction.text.get(from..end).unwrap_or("");
                    println!(
                        "     {:<24} {:.2} {}{}",
                        format!("{piece:?}"),
                        step.estimate,
                        if step.model { "model" } else { "text" },
                        if step.prose { ", prose" } else { "" },
                    );
                    from = end;
                }
            }
            None => println!("   -> nothing   ({:.0} ms)", cold.as_secs_f64() * 1e3),
        }
        if let Some((text, step)) = &engine.stopped {
            println!("   stopped before {text:?}: {step:?}");
        }
    }
}

/// Writes `--record` data: `steps.csv` with one row per token the model
/// wrote, `states.bin` with the final state before each (bf16), and for
/// every fourth case `traces.bin` with each layer's output and the last
/// layers' neurons.
struct Recorder {
    steps: std::io::BufWriter<std::fs::File>,
    states: std::io::BufWriter<std::fs::File>,
    traces: std::io::BufWriter<std::fs::File>,
    cases_file: std::io::BufWriter<std::fs::File>,
    cases: usize,
    traced: usize,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn bf16(values: &[f32], out: &mut impl std::io::Write) {
    let bytes: Vec<u8> = values.iter().flat_map(|value| ((value.to_bits() >> 16) as u16).to_le_bytes()).collect();
    out.write_all(&bytes).unwrap();
}

impl Recorder {
    fn new(directory: &Path) -> Self {
        use std::io::Write;
        std::fs::create_dir_all(directory).unwrap();
        let open = |name: &str| std::io::BufWriter::new(std::fs::File::create(directory.join(name)).unwrap());
        let mut steps = open("steps.csv");
        writeln!(steps, "case,file,placement,step,right,spelling,probability,second,entropy,agreeing,chance,typed,token,bytes,traced").unwrap();
        let mut cases = open("cases.tsv");
        writeln!(cases, "case\ttyped_hex\twritten_hex\ttruth_hex").unwrap();
        Self { steps, states: open("states.bin"), traces: open("traces.bin"), cases_file: cases, cases: 0, traced: 0 }
    }

    /// The weights needed to study the model: `weights.bin` holds as f32
    /// the token matrix, final gain and bias, output bias, then for each of
    /// the last layers expand, expand bias and reduce; `vocab.txt` one
    /// token spelling (hex) per line.
    fn write_weights(directory: &Path, network: &inline::model::Network) {
        use std::io::Write;
        let mut out = std::io::BufWriter::new(std::fs::File::create(directory.join("weights.bin")).unwrap());
        let mut put = |values: &[f32]| {
            let bytes: Vec<u8> = values.iter().flat_map(|value| value.to_le_bytes()).collect();
            out.write_all(&bytes).unwrap();
        };
        let (token, gain, bias, output) = network.output_side();
        put(&token);
        put(&gain);
        put(&bias);
        put(&output);
        for layer in network.layers() - 3..network.layers() {
            let (expand, expand_bias, reduce) = network.neuron_weights(layer);
            put(&expand);
            put(&expand_bias);
            put(&reduce);
        }
        let mut vocab = std::io::BufWriter::new(std::fs::File::create(directory.join("vocab.txt")).unwrap());
        let subwords = network.subwords();
        for token in 0..subwords.len() {
            writeln!(vocab, "{}", hex(subwords.bytes(token as u16))).unwrap();
        }
    }

    fn add(&mut self, written: &inline::model::Written, typed: usize, after: &str, placement: &str, file: usize) {
        use std::io::Write;
        if written.evidence.is_empty() || written.states.is_empty() {
            return;
        }
        self.cases += 1;
        let mut truth_end = after.len().min(300);
        while !after.is_char_boundary(truth_end) {
            truth_end -= 1;
        }
        writeln!(
            self.cases_file,
            "{}\t{}\t{}\t{}",
            self.cases,
            hex(&written.bytes[..typed.min(written.bytes.len())]),
            hex(&written.bytes),
            hex(after[..truth_end].as_bytes())
        )
        .unwrap();
        let width = written.states.len() / written.evidence.len();
        let per_trace = written.traces.len() / written.evidence.len();
        let per_neurons = written.neurons.len() / written.evidence.len();
        let trace_case = self.cases % 4 == 0;
        let mut start = 0;
        for (step, (evidence, &(end, _))) in written.evidence.iter().zip(&written.steps).enumerate() {
            let piece = &written.bytes[typed.min(end)..end];
            let spelling = end <= typed;
            let right = after.as_bytes().starts_with(piece);
            let traced = trace_case && !written.traces[step * per_trace].is_nan();
            writeln!(
                self.steps,
                "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                self.cases, file, placement, step, u8::from(right), u8::from(spelling),
                evidence.probability, evidence.second, evidence.entropy, evidence.agreeing, evidence.chance,
                evidence.typed, written.tokens[step], end - start, if traced { self.traced as i64 } else { -1 },
            )
            .unwrap();
            bf16(&written.states[step * width..(step + 1) * width], &mut self.states);
            if traced {
                bf16(&written.traces[step * per_trace..(step + 1) * per_trace], &mut self.traces);
                bf16(&written.neurons[step * per_neurons..(step + 1) * per_neurons], &mut self.traces);
                self.traced += 1;
            }
            start = end;
            if !right {
                break; // What follows a wrong token says nothing more.
            }
        }
    }
}

fn hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = rustc_hash::FxHasher::default();
    text.hash(&mut hasher);
    hasher.finish()
}

fn main() {
    let mut step = 7usize;
    let mut references = 4usize;
    let mut retrieve = 0usize;
    let mut options = inline::Options::default();
    let mut show = 0usize;
    let mut paths = Vec::new();
    let mut max_files = usize::MAX;
    let mut only_bad = false;
    let mut exclude: Vec<String> = Vec::new();
    let mut export: Option<PathBuf> = None;
    let mut model: Option<String> = None;
    let mut verify: Option<String> = None;
    let mut calibrate = false;
    let mut dump: Option<std::io::BufWriter<std::fs::File>> = None;
    let mut sample = 1usize;
    let mut cases: Option<std::io::BufWriter<std::fs::File>> = None;
    let mut tries: Vec<PathBuf> = Vec::new();
    let mut record: Option<Recorder> = None;
    let mut record_dir: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut number = |name: &str| -> f32 {
            args.next()
                .and_then(|value| value.parse().ok())
                .unwrap_or_else(|| panic!("{name} needs a number"))
        };
        match arg.as_str() {
            "--step" => step = number("--step") as usize,
            "--refs" => references = number("--refs") as usize,
            "--retrieve" => retrieve = number("--retrieve") as usize,
            "--first" => options.first = number("--first"),
            "--next" => options.next = number("--next"),
            "--block" => options.block = number("--block"),
            "--floor" => options.floor = number("--floor"),
            "--show" => show = number("--show") as usize,
            "--max-files" => max_files = number("--max-files") as usize,
            "--single-line" => options.multiline = false,
            "--max-tokens" => options.tokens = number("--max-tokens") as usize,
            "--exclude" => exclude.push(args.next().expect("--exclude needs a name")),
            "--export" => export = Some(PathBuf::from(args.next().expect("--export needs a directory"))),
            "--model" => model = Some(args.next().expect("--model needs a path")),
            "--no-model" => model = Some(String::new()),
            "--verify" => verify = Some(args.next().expect("--verify needs a vectors file")),
            "--bad" => only_bad = true,
            "--calibrate" => calibrate = true,
            "--classic" => CLASSIC.store(true, std::sync::atomic::Ordering::Relaxed),
            "--dump" => dump = Some(std::io::BufWriter::new(std::fs::File::create(args.next().expect("--dump needs a path")).unwrap())),
            // One line a case: file, line, placement, right or not, the
            // text before the caret, the truth and the guess.
            "--cases" => cases = Some(std::io::BufWriter::new(std::fs::File::create(args.next().expect("--cases needs a path")).unwrap())),
            "--budget" => options.budget = std::time::Duration::from_millis(number("--budget") as u64),
            "--sample" => sample = number("--sample") as usize,
            "--try" => tries.push(PathBuf::from(args.next().expect("--try needs a file"))),
            // With VT_RECORD set: what the model wrote at each caret, token
            // by token, labelled right or wrong, with its states.
            "--record" => {
                let directory = PathBuf::from(args.next().expect("--record needs a directory"));
                record = Some(Recorder::new(&directory));
                record_dir = Some(directory);
            }
            "--eagerness" => options = inline::Options::preset(number("--eagerness") as usize),
            // As when the user asks with the shortcut instead of typing.
            "--asked" => options = inline::Options::requested(options),
            _ => paths.push(PathBuf::from(arg)),
        }
    }
    let mut files = Vec::new();
    for path in &paths {
        collect(path, &mut files, &exclude);
    }
    if export.is_some() {
        // A language with far more files than training takes is thinned
        // before reading, evenly over its projects.
        let mut seen: std::collections::HashMap<&str, usize> = Default::default();
        for path in &files {
            *seen.entry(any_language(path)).or_default() += 1;
        }
        let mut met: std::collections::HashMap<&str, usize> = Default::default();
        files.retain(|path| {
            let name = any_language(path);
            let stride = seen[name].div_ceil(EXPORT_CAP * 3 / 2).max(1);
            let index = met.entry(name).or_default();
            *index += 1;
            (*index - 1) % stride == 0
        });
    }
    // Reading is shared between threads: a corpus is hundreds of thousands
    // of files, and opening them is what takes the time.
    let read = |path: &PathBuf| {
        if std::fs::metadata(path).ok()?.len() >= 400_000 {
            return None;
        }
        let text = std::fs::read_to_string(path).ok()?.replace("\r\n", "\n");
        (text.lines().count() > 30).then(|| (path.clone(), text))
    };
    let share = files.len().div_ceil(16).max(1);
    let texts: Vec<(PathBuf, String)> = std::thread::scope(|scope| {
        let readers: Vec<_> = files
            .chunks(share)
            .map(|part| scope.spawn(move || part.iter().filter_map(read).collect::<Vec<_>>()))
            .collect();
        readers
            .into_iter()
            .flat_map(|reader| reader.join().unwrap_or_default())
            .collect()
    });
    if let Some(directory) = export {
        export_training(&directory, &texts);
        return;
    }
    // `--model PATH` evaluates a freshly trained network; `--no-model` only
    // the pattern predictor.
    let network: Option<Option<&'static inline::model::Network>> = model.map(|path| {
        if path.is_empty() {
            return None;
        }
        let bytes = std::fs::read(&path).expect("model file");
        Some(&*Box::leak(Box::new(
            inline::model::Network::decode(&bytes).expect("valid model file"),
        )))
    });
    if !tries.is_empty() {
        for path in &tries {
            try_marks(path, network, options);
        }
        return;
    }
    if std::env::var_os("VTCODE_TIME_COLD").is_some() {
        let network = network.flatten().or_else(inline::model::embedded).expect("a model");
        let tokens = network.window() / 2;
        let mut times = inline::model::time_cold(network, tokens, 12);
        times.drain(..2);
        times.sort_by(|a, b| a.total_cmp(b));
        println!("cold {tokens}-token pass: median {:.1} ms, best {:.1} ms", times[times.len() / 2], times[0]);
        let mut times = inline::model::time_next(network, tokens, 40);
        times.sort_by(|a, b| a.total_cmp(b));
        println!("one more token: median {:.2} ms, worst {:.2} ms", times[times.len() / 2], times[times.len() - 1]);
        return;
    }
    if let Some(path) = verify {
        let vectors = std::fs::read(&path).expect("vectors file");
        let network = network.flatten().or_else(inline::model::embedded).expect("a model");
        match network.difference(&vectors) {
            Some(worst) => println!("largest probability difference from the trainer: {worst:.2e}"),
            None => println!("the vectors do not fit this model"),
        }
        return;
    }
    if let (Some(_), Some(directory)) = (record.as_ref(), record_dir.as_ref()) {
        let network = network.flatten().or_else(inline::model::embedded).expect("a model");
        Recorder::write_weights(directory, network);
    }
    let mut retriever = retrieval::Retriever::default();
    if retrieve > 0 {
        for (at, (_, reference)) in texts.iter().enumerate() {
            retriever.add(at as u64, reference);
        }
        println!("retrieval index: {} files", retriever.len());
    }
    let stride = (texts.len() / max_files.min(texts.len()).max(1)).max(1);
    let names = [
        "append/start", "append/typed", "append/mid", "append/open",
        "insert/start", "insert/typed", "insert/mid", "insert/open",
        "comment/typed", "comment/mid",
    ];
    let mut tallies = vec![Tally::default(); names.len()];
    let mut times = Vec::new();
    let mut ranks = (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut shown_examples = 0usize;
    let mut buckets: std::collections::BTreeMap<Bucket, (usize, usize)> = Default::default();
    for (index, (path, text)) in texts.iter().enumerate().step_by(stride) {
        let syntax = inline::Syntax::for_language(any_language(path));
        let mut engine = inline::Engine::new();
        if let Some(network) = network {
            engine.set_network(network);
        }
        // Neighbouring files of the same kind play the role of open tabs.
        let mut added = 0;
        for offset in 1..texts.len() {
            if added == references {
                break;
            }
            let (other, reference) = &texts[(index + offset) % texts.len()];
            if other.extension() == path.extension() && other.parent() == path.parent() {
                engine.set_reference(hash(reference), reference, syntax);
                added += 1;
            }
        }
        let mut current: Vec<usize> = Vec::new();
        let lines: Vec<&str> = text.split('\n').collect();
        let mut offsets = Vec::with_capacity(lines.len());
        let mut offset = 0;
        for line in &lines {
            offsets.push(offset);
            offset += line.len() + 1;
        }
        for line in (12..lines.len()).step_by(step) {
            let source = lines[line];
            let content = source.trim_start();
            let trimmed = content.trim_end();
            if trimmed.len() < 4 {
                continue;
            }
            let comment = ["//", "#", "*", "/*", "--"].iter().any(|sign| content.starts_with(sign));
            // In a comment the typing starts after its sign.
            let sign = if comment {
                content.len() - content.trim_start_matches(|ch: char| !ch.is_alphanumeric()).len()
            } else {
                0
            };
            if comment && trimmed.len() < sign + 8 {
                continue;
            }
            let indent = source.len() - content.len();
            let typed = content[sign..]
                .char_indices()
                .nth(3)
                .map_or(content.len(), |(at, _)| sign + at);
            let mut half = trimmed.len() * 2 / 5;
            while !trimmed.is_char_boundary(half) {
                half += 1;
            }
            let mid = trimmed[half..]
                .find(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
                .map(|at| half + at + 1)
                .filter(|&at| at < trimmed.len() && trimmed.is_char_boundary(at));
            let below_start = (offsets[line] + source.len() + 1).min(text.len());
            let modes: &[(usize, &str)] = if comment {
                &[(7, "")]
            } else {
                &[(0, ""), (4, &text[below_start..])]
            };
            for &(mode, below) in modes {
                let open_end = trimmed
                    .ends_with(['{', '(', '['])
                    .then_some(indent + trimmed.len());
                for (placement, column) in [
                    (0, (!comment).then_some(indent)),
                    (1, Some(indent + typed)),
                    (2, mid.map(|m| indent + m)),
                    (3, open_end.filter(|_| !comment)),
                ] {
                    let Some(column) = column else { continue };
                    if column >= indent + trimmed.len() && placement != 3 {
                        continue;
                    }
                    let caret = offsets[line] + column;
                    let before = &text[..caret];
                    let truth_line = source[column..].trim_end();
                    let truth_all = text[caret..].trim_end();
                    let started = Instant::now();
                    if retrieve > 0 {
                        // Workspace retrieval: the files that share the most
                        // distinctive names with the code around the caret.
                        let mut from = caret.saturating_sub(3000);
                        while !text.is_char_boundary(from) {
                            from += 1;
                        }
                        let mut to = (below_start + 1000).min(text.len());
                        while !text.is_char_boundary(to) {
                            to -= 1;
                        }
                        let context = format!("{}{}", &text[from..caret], &text[below_start.min(to)..to]);
                        let wanted: Vec<usize> = retriever
                            .query(&context, index as u64, retrieve)
                            .into_iter()
                            .map(|key| key as usize)
                            .collect();
                        for &old in &current {
                            if !wanted.contains(&old) {
                                engine.remove_reference(hash(&texts[old].1));
                            }
                        }
                        for &file in &wanted {
                            let reference = &texts[file].1;
                            if engine.has_reference(hash(reference)) {
                                engine.touch_reference(hash(reference));
                            } else {
                                let syntax = inline::Syntax::for_language(any_language(&texts[file].0));
                                engine.set_reference(hash(reference), reference, syntax);
                            }
                        }
                        current = wanted;
                    }
                    let caret = inline::Caret {
                        key: index as u64,
                        before,
                        rest: "",
                        below,
                        syntax,
                        language: inline::language_id(any_language(path)),
                    };
                    // As in the editor, the model has read the text up to
                    // the caret line before the request: it reads ahead
                    // when the caret comes to rest and after every answer.
                    // That reading is not part of the request's latency.
                    let reading = Instant::now();
                    engine.warm(
                        inline::Caret {
                            before: &before[..before.rfind('\n').unwrap_or(0)],
                            ..caret
                        },
                        &|| false,
                    );
                    let read = reading.elapsed();
                    let prediction = engine.predict(caret, options, &|| false);
                    times.push((started.elapsed() - read).as_secs_f64() * 1e6);
                    if let (Some(record), Some((typed, written))) = (record.as_mut(), engine.first_written.take()) {
                        record.add(&written, typed, &text[caret.before.len()..], names[mode + placement], index);
                    }
                    // How the weighing against the text below chose, when it
                    // chose another line than the greedy one.
                    if let Some((typed, greedy, chosen)) = inline::model::RANKED.with(|ranked| ranked.borrow_mut().take()) {
                        let after = text[caret.before.len()..].as_bytes();
                        let line_right = |line: &[u8]| {
                            let line = &line[typed.min(line.len())..];
                            let content = &line[..line.iter().position(|&b| b == b'\n').unwrap_or(line.len())];
                            after.starts_with(content) && after.get(content.len()) == Some(&b'\n')
                        };
                        ranks.0 += 1;
                        if greedy != chosen {
                            ranks.1 += 1;
                            ranks.2 += usize::from(line_right(&greedy));
                            ranks.3 += usize::from(line_right(&chosen));
                        } else {
                            ranks.4 += usize::from(line_right(&greedy));
                        }
                    }
                    let tally = &mut tallies[mode + placement];
                    tally.cases += 1;
                    tally.truth_chars += if placement == 3 {
                        lines.get(line + 1).map_or(0, |next| next.trim().len())
                    } else {
                        truth_line.len()
                    };
                    if let Some(cases) = cases.as_mut() {
                        use std::io::Write;
                        let guess = prediction.as_ref().map_or("", |prediction| prediction.text.as_str());
                        let right = !guess.is_empty() && text[caret.before.len()..].starts_with(guess);
                        writeln!(
                            cases,
                            "{}	{}	{}	{}	{:?}	{:?}	{:?}	{:?}",
                            path.display(), line + 1, names[mode + placement], u8::from(right), &source[..column], truth_line, guess,
                            engine.stopped
                        )
                        .unwrap();
                    }
                    let Some(prediction) = prediction else { continue };
                    tally.shown += 1;
                    let multi = prediction.text.contains('\n');
                    tally.multi_shown += usize::from(multi);
                    let normalized = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
                    let correct = if multi {
                        normalized(truth_all).starts_with(&normalized(&prediction.text))
                    } else {
                        truth_line.starts_with(prediction.text.as_str())
                    };
                    if let Some(dump) = dump.as_mut() {
                        use std::io::Write;
                        let truth_norm = normalized(truth_all);
                        for entry in &prediction.trace {
                            let piece = &prediction.text[..entry.end.min(prediction.text.len())];
                            let ok = if piece.contains('\n') {
                                truth_norm.starts_with(&normalized(piece))
                            } else {
                                truth_line.starts_with(piece)
                            };
                            let features: Vec<String> = entry.features().iter().map(|f| f.to_string()).collect();
                            writeln!(dump, "{},{},{}", u8::from(ok), index % 2, features.join(",")).unwrap();
                            if !ok { break; }
                        }
                    }
                    if calibrate {
                        let truth_norm = normalized(truth_all);
                        for (step, entry) in prediction.trace.iter().enumerate() {
                            let piece = &prediction.text[..entry.end.min(prediction.text.len())];
                            let ok = if piece.contains('\n') {
                                truth_norm.starts_with(&normalized(piece))
                            } else {
                                truth_line.starts_with(piece)
                            };
                            let supporters = match entry.supporters { 1 => 1, 2 => 2, 3..=4 => 3, _ => 5 };
                            let score = match entry.score as usize { 0..=5 => 0, 6..=9 => 6, 10..=15 => 10, 16..=23 => 16, _ => 24 };
                            let share = (entry.estimate * 10.).min(9.) as usize;
                            let entry_typed = entry.typed > 0;
                            let _ = (supporters, score);
                            let slot = buckets.entry((step == 0, if entry_typed { 1 } else { 0 }, 0, 0, entry.lines > 0, share)).or_default();
                            slot.0 += 1;
                            slot.1 += usize::from(ok);
                            if !ok { break; }
                        }
                    }
                    if correct {
                        tally.correct += 1;
                        tally.multi_correct += usize::from(multi);
                        tally.saved += prediction.text.len().min(truth_all.len());
                        tally.full += usize::from(prediction.text.len() >= truth_line.len());
                    } else {
                        tally.wrong_chars += prediction.text.len();
                    }
                    if shown_examples < show && (mode + placement) % 4 != 1 && (!only_bad || !correct) && (line * 7 + placement) % sample == 0 {
                        shown_examples += 1;
                        println!(
                            "{} {}:{} [{}] conf {:.2}\n  before: {:?}\n  truth:  {:?}\n  guess:  {:?}",
                            if correct { "OK " } else { "BAD" },
                            path.display(),
                            line + 1,
                            names[mode + placement],
                            prediction.confidence,
                            &source[..column],
                            truth_line,
                            prediction.text
                        );
                    }
                }
            }
        }
    }
    if calibrate {
        println!("first kind supp score nl share      n   acc%");
        for ((first, kind, supporters, score, newline, share), (n, ok)) in &buckets {
            if *n >= 25 {
                println!("{:5} {:4} {:4} {:5} {:2} {:5} {:6} {:6.1}", u8::from(*first), kind, supporters, score, u8::from(*newline), share, n, 100. * *ok as f64 / *n as f64);
            }
        }
    }
    println!(
        "{:<14} {:>6} {:>7} {:>9} {:>8} {:>10} {:>9} {:>11}",
        "placement", "cases", "shown%", "correct%", "full%", "saved/case", "multi", "multi-ok%"
    );
    for (name, tally) in names.iter().zip(&tallies) {
        let percent = |a: usize, b: usize| if b == 0 { 0. } else { 100. * a as f64 / b as f64 };
        println!(
            "{:<14} {:>6} {:>7.1} {:>9.1} {:>8.1} {:>9.1}% {:>9} {:>11.1}",
            name,
            tally.cases,
            percent(tally.shown, tally.cases),
            percent(tally.correct, tally.shown),
            percent(tally.full, tally.shown),
            percent(tally.saved, tally.truth_chars),
            tally.multi_shown,
            percent(tally.multi_correct, tally.multi_shown),
        );
    }
    // All code placements together (comments aside): shown, right when
    // shown, right of all cases, whole line right of all cases, saved.
    let code: Vec<&Tally> = names.iter().zip(&tallies).filter(|(name, _)| !name.starts_with("comment")).map(|(_, tally)| tally).collect();
    let sum = |field: fn(&Tally) -> usize| code.iter().map(|tally| field(tally)).sum::<usize>() as f64;
    let (cases, shown, correct) = (sum(|t| t.cases), sum(|t| t.shown), sum(|t| t.correct));
    println!(
        "code overall: cases {cases}  shown {:.1}%  right {:.1}%  correct positions {:.1}%  whole line {:.1}%  saved {:.1}%",
        100. * shown / cases.max(1.),
        100. * correct / shown.max(1.),
        100. * correct / cases.max(1.),
        100. * sum(|t| t.full) / cases.max(1.),
        100. * sum(|t| t.saved) / sum(|t| t.truth_chars).max(1.),
    );
    if ranks.0 > 0 {
        println!(
            "weighed against the text below: {} lines, {} switched (greedy right {}, chosen right {}), kept right {}",
            ranks.0, ranks.1, ranks.2, ranks.3, ranks.4
        );
    }
    times.sort_by(|a, b| a.total_cmp(b));
    if !times.is_empty() {
        let at = |q: f64| times[((times.len() - 1) as f64 * q) as usize];
        println!(
            "latency µs: median {:.0}  p95 {:.0}  p99 {:.0}  max {:.0}  (n={})",
            at(0.5),
            at(0.95),
            at(0.99),
            at(1.0),
            times.len()
        );
    }
}
