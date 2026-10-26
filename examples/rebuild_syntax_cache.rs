#[allow(dead_code)]
#[path = "../src/syntax_assets.rs"]
mod syntax_assets;
#[allow(dead_code)]
#[path = "../src/vscode_grammar.rs"]
mod vscode_grammar;

fn main() {
    let folder = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("syntaxes"));
    match syntax_assets::rebuild_syntax_cache(&folder) {
        Ok((count, fingerprint)) => println!(
            "cached {count} syntaxes from {} (fingerprint {fingerprint:016x})",
            folder.display()
        ),
        Err(error) => {
            eprintln!("failed to rebuild syntax cache: {error}");
            std::process::exit(1);
        }
    }
}
