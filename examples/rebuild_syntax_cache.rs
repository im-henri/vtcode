#[allow(dead_code)]
#[path = "../src/scope_selector.rs"]
mod scope_selector;
#[allow(dead_code)]
#[path = "../src/syntax_assets.rs"]
mod syntax_assets;
#[allow(dead_code)]
#[path = "../src/textmate.rs"]
mod textmate;
#[allow(dead_code)]
#[path = "../src/textmate_grammar.rs"]
mod textmate_grammar;
#[allow(dead_code)]
#[path = "../src/textmate_theme.rs"]
mod textmate_theme;

fn main() {
    let folder = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("grammars"));
    match syntax_assets::rebuild_syntax_cache(&folder) {
        Ok((count, fingerprint)) => println!(
            "cached {count} grammars from {} (fingerprint {fingerprint:016x})",
            folder.display()
        ),
        Err(error) => {
            eprintln!("failed to rebuild syntax cache: {error}");
            std::process::exit(1);
        }
    }
}
