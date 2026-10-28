//! Local syntax installation. Parsing and disk access run on a worker.
use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use crate::textmate::SyntaxRegistry;

const MAX_SYNTAX_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct InstalledSyntax {
    pub name: String,
}

pub(crate) fn source_path(input: &str, workspace: &Path) -> Result<PathBuf, String> {
    let input = input.trim();
    let input = input
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| input.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(input);
    if input.is_empty() {
        return Err("Enter the path to a TextMate .tmLanguage.json or .tmLanguage file.".into());
    }
    if input.contains("://")
        || input
            .get(..5)
            .is_some_and(|s| s.eq_ignore_ascii_case("http:"))
        || input
            .get(..6)
            .is_some_and(|s| s.eq_ignore_ascii_case("https:"))
    {
        return Err("Use a local syntax file path. Web links are not supported.".into());
    }
    let path = Path::new(input);
    if !path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("tmLanguage") || e.eq_ignore_ascii_case("json") || e.eq_ignore_ascii_case("plist"))
    {
        return Err("Choose a TextMate .tmLanguage.json or .tmLanguage file.".into());
    }
    Ok(if path.is_absolute() {
        path.to_owned()
    } else {
        workspace.join(path)
    })
}

pub(crate) fn install(
    source: &Path,
    root: &Path,
    syntaxes: &SyntaxRegistry,
) -> Result<InstalledSyntax, String> {
    let read_error = |error| format!("Could not read {}: {error}", source.display());
    let file = File::open(source).map_err(read_error)?;
    let metadata = file.metadata().map_err(read_error)?;
    if !metadata.is_file() {
        return Err("Choose a syntax file, not a folder.".into());
    }
    if metadata.len() > MAX_SYNTAX_BYTES {
        return Err("Syntax file exceeds 16 MiB.".into());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_SYNTAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(read_error)?;
    if bytes.len() as u64 > MAX_SYNTAX_BYTES {
        return Err("Syntax file exceeds 16 MiB.".into());
    }
    let definition = crate::textmate_grammar::parse_source(&bytes,source.file_stem().and_then(|s|s.to_str()).unwrap_or("TextMate"))
        .map_err(|e|format!("Invalid TextMate grammar: {e}"))?;
    let name=definition.name.clone();
    let mut builder=syntaxes.clone().into_builder();
    builder.add(definition.clone());
    let validated=builder.build()?;
    crate::textmate_grammar::validate_references(&definition,&validated)?;

    let filename = source
        .file_name()
        .ok_or("The syntax file needs a filename.")?;
    let destination = root.join(filename);
    std::fs::create_dir_all(root).map_err(|e| format!("Could not create syntax directory: {e}"))?;
    // Validate before replacing anything, and publish one complete file. A
    // failed install leaves the existing grammar and source file untouched.
    let mut staging = tempfile::NamedTempFile::new_in(root).map_err(|e| e.to_string())?;
    staging
        .write_all(&bytes)
        .map_err(|e| e.to_string())?;
    staging.flush().map_err(|e| e.to_string())?;
    staging
        .persist(&destination)
        .map_err(|e| format!("Could not install syntax: {}", e.error))?;
    Ok(InstalledSyntax { name })
}
