//! Local syntax installation. Parsing and disk access run on a worker.
use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use syntect::parsing::{
    SyntaxDefinition, SyntaxSet,
    syntax_definition::{ContextReference, MatchOperation, Pattern},
};

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
        return Err("Enter the path to a .sublime-syntax or TextMate .json file.".into());
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
        .is_some_and(|e| e.eq_ignore_ascii_case("sublime-syntax") || e.eq_ignore_ascii_case("json"))
    {
        return Err("Choose a .sublime-syntax or TextMate .json file.".into());
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
    syntaxes: &SyntaxSet,
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
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| "Syntax files must use UTF-8.".to_string())?
        .trim_start_matches('\u{feff}');
    let text = if source
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
    {
        crate::vscode_grammar::standalone_sublime_source(text)?
    } else {
        text.to_owned()
    };
    let definition =
        SyntaxDefinition::load_from_str(&text, true, source.file_stem().and_then(|s| s.to_str()))
            .map_err(|e| format!("Invalid syntax file: {e}"))?;
    let name = definition.name.clone();
    let mut builder = syntaxes.clone().into_builder();
    builder.add(definition);
    let validated = builder.build();
    let warnings: Vec<_> = validated
        .warnings()
        .iter()
        .filter(|warning| !syntaxes.warnings().contains(warning))
        .cloned()
        .collect();
    if !warnings.is_empty() {
        return Err(format!(
            "Syntax could not be loaded: {}",
            warnings.join("\n")
        ));
    }
    // Syntect leaves missing includes unresolved without adding a warning.
    // Inspect the linked definition, including inherited contexts, before
    // publishing it. Existing grammars' optional dependencies are unaffected.
    let linked = validated.into_builder();
    validate_references(linked.syntaxes().last().expect("added syntax"))?;

    let filename = source
        .file_name()
        .ok_or("The syntax file needs a filename.")?;
    let destination = root.join(filename).with_extension("sublime-syntax");
    std::fs::create_dir_all(root).map_err(|e| format!("Could not create syntax directory: {e}"))?;
    // Validate before replacing anything, and publish one complete file. A
    // failed install leaves the existing grammar and source file untouched.
    let mut staging = tempfile::NamedTempFile::new_in(root).map_err(|e| e.to_string())?;
    staging
        .write_all(text.as_bytes())
        .map_err(|e| e.to_string())?;
    staging.flush().map_err(|e| e.to_string())?;
    staging
        .persist(&destination)
        .map_err(|e| format!("Could not install syntax: {}", e.error))?;
    Ok(InstalledSyntax { name })
}

fn validate_references(syntax: &SyntaxDefinition) -> Result<(), String> {
    let check = |reference: &ContextReference| {
        if matches!(reference, ContextReference::Direct { .. }) {
            Ok(())
        } else {
            Err(format!(
                "Syntax '{}' references an unavailable context: {reference:?}. Install its required syntax first.",
                syntax.name
            ))
        }
    };
    for context in syntax.contexts.values() {
        for pattern in &context.patterns {
            match pattern {
                Pattern::Include(reference) | Pattern::IncludeWithPrototype(reference) => {
                    check(reference)?;
                }
                Pattern::Match(pattern) => {
                    if let Some(reference) = &pattern.with_prototype {
                        check(reference)?;
                    }
                    let references = match &pattern.operation {
                        MatchOperation::Push(references)
                        | MatchOperation::Set {
                            ctx_refs: references,
                            ..
                        }
                        | MatchOperation::Branch {
                            alternatives: references,
                            ..
                        }
                        | MatchOperation::Embed {
                            contexts: references,
                            ..
                        } => references.as_slice(),
                        MatchOperation::None | MatchOperation::Pop(_) | MatchOperation::Fail(_) => {
                            &[]
                        }
                    };
                    for reference in references {
                        check(reference)?;
                    }
                }
            }
        }
    }
    Ok(())
}
