//! Opt-in integration tests against installed language servers, never mocks.
//! cargo test --bin vtcode real_lsp_ -- --ignored --test-threads=1 --nocapture
use super::*;
use std::time::{Duration, Instant};

struct Session {
    manager: LspManager,
    _directory: tempfile::TempDir,
    root: PathBuf,
    path: PathBuf,
    source: String,
}

fn wait(manager: &mut LspManager, label: &str, ready: impl Fn(&LspManager) -> bool) {
    let start = Instant::now();
    loop {
        manager.poll();
        if ready(manager) {
            eprintln!("LSP {label}: {} ms", start.elapsed().as_millis());
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(25),
            "timed out: {label}; errors={:?}; startup={:?}",
            manager.request_errors,
            manager.startup_errors
        );
        assert!(
            !manager.clients.is_empty(),
            "server disconnected: {label}: {:?}",
            manager.startup_errors
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn point(text: &str, needle: &str, within: usize) -> (u32, u32) {
    let offset = text
        .find(needle)
        .unwrap_or_else(|| panic!("missing {needle:?}"))
        + within;
    let before = &text[..offset];
    (
        before.matches('\n').count() as u32,
        before.rsplit('\n').next().unwrap().encode_utf16().count() as u32,
    )
}

impl Session {
    fn new(language: &str, source: &str) -> Self {
        Self::configured(language, source, None, &[], 0)
    }

    fn configured(
        language: &str,
        source: &str,
        settings: Option<Value>,
        files: &[(&str, &str)],
        startup_edits: u64,
    ) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("workspace with spaces 世界");
        std::fs::create_dir_all(&root).unwrap();
        let python = language == "python";
        let extension = if python {
            "py"
        } else if language == "c" {
            "c"
        } else {
            "cpp"
        };
        let path = root.join(format!("example.{extension}"));
        std::fs::write(&path, source).unwrap();
        if python {
            std::fs::write(
                root.join("pyrightconfig.json"),
                r#"{"include":["."],"typeCheckingMode":"basic","pythonVersion":"3.13"}"#,
            )
            .unwrap();
        }
        for (name, content) in files {
            std::fs::create_dir_all(root.join(name).parent().unwrap()).unwrap();
            std::fs::write(root.join(name), content).unwrap();
        }
        let mut manager = LspManager::new(root.clone());
        manager.register_config(LspServerConfig {
            language: language.into(),
            command: std::env::var(if python { "VTCODE_PYRIGHT_LANGSERVER" } else { "VTCODE_CLANGD" })
                .unwrap_or_else(|_| if python { "pyright-langserver" } else { "clangd" }.into()),
            args: if python { vec!["--stdio".into()] } else { vec!["--background-index=false".into(), "--clang-tidy=false".into(), "--header-insertion=never".into()] },
            file_patterns: if python { vec!["*.py".into()] } else { vec!["*.c".into(), "*.cpp".into(), "*.h".into(), "*.hpp".into()] },
            settings, init_options: (!python).then(|| serde_json::json!({"fallbackFlags":[if language == "c" {"-std=c11"} else {"-std=c++17"}]})),
        });
        assert!(
            manager.ensure_client_for_file(&path).is_some(),
            "Install the requested server or set VTCODE_PYRIGHT_LANGSERVER/VTCODE_CLANGD: {:?}",
            manager.startup_errors
        );
        manager.sync_document_owned(&path, 1, source.to_owned());
        for version in 2..=startup_edits + 1 {
            manager.sync_document_owned(
                &path,
                version,
                if version == startup_edits + 1 {
                    source.to_owned()
                } else {
                    "deliberately invalid source".into()
                },
            );
        }
        // Early passive requests must not poison the cache before capabilities
        // arrive. Later outline/inlay checks run against the same unchanged file.
        assert!(!manager.request_document_symbol(&path));
        assert!(!manager.request_inlay_hints(&path, 0, 7));
        assert!(manager.document_symbol_request.is_none());
        assert!(manager.inlay_hint_request.is_none());
        wait(&mut manager, "initialize", |m| m.clients[0].initialized);
        Self {
            manager,
            _directory: directory,
            root,
            path,
            source: source.into(),
        }
    }

    fn position(&self, needle: &str, within: usize) -> (u32, u32) {
        point(&self.source, needle, within)
    }

    fn diagnostics(&mut self, errors: bool) {
        let path = self.path.clone();
        wait(
            &mut self.manager,
            if errors {
                "error diagnostics"
            } else {
                "clean diagnostics"
            },
            |m| {
                m.current_diagnostic_context(&path).is_some()
                    && m.diagnostic_publication.next_wait(Instant::now()).is_none()
                    && m.get_diagnostics(&path).is_some_and(|items| {
                        items
                            .iter()
                            .any(|d| d.severity == DiagnosticSeverity::ERROR)
                            == errors
                    })
            },
        );
    }

    fn change(&mut self, source: String, version: u64) {
        self.source = source;
        self.manager.invalidate_document_feedback(&self.path);
        self.manager
            .sync_document_owned(&self.path, version, self.source.clone());
    }
}

const PYTHON: &str = "def sum_values(left: int, right: int) -> int:\n    return left + right\n\nvalue = sum_values(2, 3)\nwrong: int = \"wrong\"\n";
const C: &str = "int sum_values(int left, int right) {\n    return left + right;\n}\nint caller(void) {\n    int result = sum_values(2, 3);\n    return result;\n}\nint invalid(void) { return missing_symbol; }\n";

fn associated_file_workflow(language: &str) {
    // clangd uses the project's compile command, not LSP languageId, to parse
    // custom extensions. Preserve that contract instead of overriding build flags.
    let clang_config = format!("If:\n  PathMatch: '.*\\.custom-type'\nCompileFlags:\n  Add: [-x{}]\n", if language == "c" { "c" } else { "c++" });
    let files = [(".clangd", clang_config.as_str())];
    let mut session = Session::configured(language, if language == "python" { PYTHON } else { C }, None,
        if language == "python" { &[] } else { &files }, 0);
    let path = session.root.join("associated.custom-type");
    std::fs::write(&path, &session.source).unwrap();
    session.manager.file_associations = Arc::new([
        ("*.custom-type".into(), language.into()),
    ].into_iter().collect());
    for client in &mut session.manager.clients {
        client.file_associations = Arc::clone(&session.manager.file_associations);
    }
    assert_eq!(session.manager.ensure_client_for_file(&path), Some(0));
    assert_eq!(session.manager.clients[0].document_language(&path), language);
    session.manager.sync_document_owned(&path, 1, session.source.clone());
    let (line, col) = session.position("sum_values(2", 3);
    assert!(session.manager.request_hover(&path, line, col));
    wait(&mut session.manager, "associated-file hover", |manager| manager.hover_content.is_some());
    assert!(session.manager.hover_content.as_ref().unwrap().contains("sum_values"));
    assert!(session.manager.request_completion(&path, line, col, "sum"));
    wait(&mut session.manager, "associated-file completion", |manager| !manager.completion_items.is_empty());
    assert!(session.manager.completion_items.iter().any(|item| item.label.contains("sum_values")));
}

#[test]
#[ignore = "requires installed pyright-langserver"]
fn real_lsp_pyright_file_associations() { associated_file_workflow("python"); }

#[test]
#[ignore = "requires installed clangd"]
fn real_lsp_clangd_c_file_associations() { associated_file_workflow("c"); }

#[test]
#[ignore = "requires installed clangd"]
fn real_lsp_clangd_cpp_file_associations() { associated_file_workflow("cpp"); }

fn diagnostics_during_completion(language: &str) {
    let python = language == "python";
    let clean = if python { "digest = 1\nprint(digest)\n" }
        else { "int digest = 1;\nint main(void) { return digest; }\n" };
    let partial = if python { clean.replace("print(digest)", "print(dige)") }
        else { clean.replace("return digest;", "return dige;") };
    let mut session = Session::new(language, clean);
    session.diagnostics(false);
    let published = session.manager.diagnostics_version;
    let path = session.path.clone();
    // Leave enough room for real server work without sleeping for the delay.
    session.manager.diagnostic_publication.set_delay_ms(10_000);
    session.change(partial.clone(), 2);
    let (line, col) = session.position(if python { "print(dige)" } else { "return dige;" }, if python { 10 } else { 11 });
    session.manager.request_completion(&path, line, col, "dige");
    wait(&mut session.manager, "completion and buffered diagnostics", |manager| {
        manager.completion_items.iter().any(|item| item.label.contains("digest"))
            && manager.current_diagnostic_context(&path).is_some_and(|items| {
                items.iter().any(|item| item.severity == Some(DiagnosticSeverity::ERROR))
            })
    });
    assert_eq!(session.manager.diagnostics_version, published);
    assert!(session.manager.get_diagnostics(&path).unwrap().is_empty());
    assert!(session.manager.diagnostic_publication.next_wait(Instant::now()).is_some());
    session.change(clean.into(), 3);
    wait(&mut session.manager, "corrected diagnostic context", |manager| {
        manager.current_diagnostic_context(&path).is_some_and(|items| items.is_empty())
    });
    assert_eq!(session.manager.diagnostics_version, published, "unfinished code never became a displayed problem");
    session.manager.publish_pending_diagnostics(Instant::now() + Duration::from_secs(10));
    assert!(session.manager.get_diagnostics(&path).unwrap().is_empty());
    assert_eq!(session.manager.diagnostics_version, published, "unchanged clean diagnostics keep render caches intact");

    // A real error left in place still appears after the normal quiet period.
    session.manager.diagnostic_publication.set_delay_ms(super::diagnostics::DEFAULT_DELAY_MS);
    session.change(partial, 4);
    session.diagnostics(true);
}

#[test]
#[ignore = "requires installed pyright-langserver"]
fn real_lsp_pyright_defers_problems_while_completing_partial_words() {
    diagnostics_during_completion("python");
}

#[test]
#[ignore = "requires installed clangd"]
fn real_lsp_clangd_c_defers_problems_while_completing_partial_words() {
    diagnostics_during_completion("c");
}

#[test]
#[ignore = "requires installed clangd"]
fn real_lsp_clangd_cpp_defers_problems_while_completing_partial_words() {
    diagnostics_during_completion("cpp");
}

fn editing_workflow(language: &str, source: &str) {
    let mut s = Session::new(language, source);
    s.diagnostics(true);
    let (line, col) = s.position("sum_values(2", 3);
    assert!(s.manager.request_completion(&s.path, line, col, "sum"));
    wait(&mut s.manager, "completion", |m| {
        !m.completion_items.is_empty()
    });
    assert!(
        s.manager
            .completion_items
            .iter()
            .any(|item| item.label.contains("sum_values")),
        "{:?}",
        s.manager.completion_items
    );
    let index = s.manager.completion_items.iter()
        .position(|item| item.label.contains("sum_values")).unwrap();
    let resolves = s.manager.clients[0].supports_request("completionItem/resolve");
    assert!(s.manager.request_completion_resolve(&s.path, index));
    wait(&mut s.manager, "completion details", |manager| manager.active_resolve_id.is_none());
    if resolves { assert!(s.manager.completion_resolve_dirty); }
    for _ in 0..32 {
        assert!(s.manager.request_completion_resolve(&s.path, index));
        assert!(s.manager.active_resolve_id.is_none(), "resolved items must stay cached");
    }
    assert!(s.manager.request_hover(&s.path, line, col));
    wait(&mut s.manager, "hover", |m| m.hover_content.is_some());
    assert!(
        s.manager
            .hover_content
            .as_ref()
            .unwrap()
            .contains("sum_values")
    );
    assert!(s.manager.request_definition(&s.path, line, col));
    wait(&mut s.manager, "definition", |m| {
        !m.definition_locations.is_empty()
    });
    assert!(
        s.manager
            .definition_locations
            .iter()
            .any(|l| l.range.start.line == 0 && l.uri.to_file_path().ok() == Some(s.path.clone()))
    );
    assert!(s.manager.request_references(&s.path, line, col));
    wait(&mut s.manager, "references", |m| {
        m.reference_locations.len() >= 2
    });
    let (line, col) = s.position("sum_values(2,", "sum_values(2,".len());
    assert!(s.manager.request_signature_help(&s.path, line, col));
    wait(&mut s.manager, "signature help", |m| {
        m.signature_help.is_some()
    });
    let signature = s.manager.signature_help.as_ref().unwrap();
    assert!(
        signature
            .signatures
            .iter()
            .any(|s| s.label.contains("right"))
    );
    assert_eq!(signature.active_parameter, 1);
    assert!(s.manager.request_document_symbol(&s.path));
    wait(&mut s.manager, "outline", |m| m.document_symbols_dirty);
    assert!(
        s.manager
            .document_symbols
            .iter()
            .any(|item| item.name == "sum_values")
    );
    assert!(s.manager.request_document_highlight(
        &s.path,
        0,
        if language == "python" { 5 } else { 6 }
    ));
    wait(&mut s.manager, "document highlights", |m| {
        m.document_highlights_dirty
    });
    assert!(s.manager.document_highlights.len() >= 2);
    let (line, col) = s.position("sum_values(2", 3);
    assert!(s.manager.request_rename(&s.path, line, col, "total_values"));
    wait(&mut s.manager, "rename", |m| m.rename_edit.is_some());
    let edit = serde_json::to_string(s.manager.rename_edit.as_ref().unwrap()).unwrap();
    assert!(edit.matches("total_values").count() >= 2, "{edit}");
    let fixed = s
        .source
        .replace("\"wrong\"", "42")
        .replace("missing_symbol", "42");
    s.change(fixed.clone(), 2);
    s.diagnostics(false);
    assert_eq!(
        std::fs::read_to_string(&s.path).unwrap(),
        source,
        "unsaved updates must not touch disk"
    );
    for version in 3..=32 {
        let text = if version == 32 {
            source.to_owned()
        } else {
            fixed.clone()
        };
        s.change(text, version);
    }
    s.diagnostics(true);
    s.change(fixed, 33);
    s.diagnostics(false);
    s.manager.did_close(&s.path);
    assert!(s.manager.get_diagnostics(&s.path).is_none());
    s.manager
        .sync_document_owned(&s.path, 34, source.to_owned());
    s.diagnostics(true);
    assert!(
        s.manager.request_errors.is_empty(),
        "{:?}",
        s.manager.request_errors
    );
    s.manager.shutdown_all();
    assert!(s.manager.clients.is_empty());
}

#[test]
#[ignore = "requires installed pyright-langserver"]
fn real_lsp_pyright_editing_workflow() {
    editing_workflow("python", PYTHON);
}

#[test]
#[ignore = "requires installed clangd"]
fn real_lsp_clangd_c_editing_workflow() {
    editing_workflow("c", C);
}

#[test]
#[ignore = "requires installed clangd"]
fn real_lsp_clangd_cpp_editing_workflow() {
    editing_workflow("cpp", C);
}

#[test]
#[ignore = "requires installed clangd"]
fn real_lsp_clangd_cpp_rich_features() {
    let mut s = Session::new("cpp", C);
    s.diagnostics(true);
    assert!(s.manager.request_format_document(&s.path, 4, true));
    wait(&mut s.manager, "format document", |m| m.format_dirty);
    assert!(!s.manager.format_edits.is_empty());
    assert!(s.manager.request_semantic_tokens_full(&s.path));
    wait(&mut s.manager, "semantic tokens", |m| {
        m.semantic_tokens_dirty
    });
    assert!(
        !s.manager
            .semantic_tokens_response
            .as_ref()
            .unwrap()
            .data
            .is_empty()
    );
    assert!(s.manager.request_folding_range(&s.path));
    wait(&mut s.manager, "folding", |m| m.folding_ranges_dirty);
    assert!(!s.manager.folding_ranges.is_empty());
    assert!(
        s.manager
            .request_selection_range(&s.path, vec![Position::new(4, 20)])
    );
    wait(&mut s.manager, "selection ranges", |m| {
        m.selection_ranges_dirty
    });
    assert!(s.manager.selection_ranges[0].parent.is_some());
    assert!(s.manager.request_inlay_hints(&s.path, 0, 7));
    wait(&mut s.manager, "inlay hints", |m| m.inlay_hints_dirty);
    assert!(!s.manager.inlay_hints.is_empty());
    assert!(s.manager.request_prepare_call_hierarchy(&s.path, 0, 6));
    wait(&mut s.manager, "call hierarchy", |m| m.call_hierarchy_dirty);
    let item = s.manager.call_hierarchy_items[0].clone();
    assert!(s.manager.request_incoming_calls(&item));
    wait(&mut s.manager, "incoming calls", |m| {
        m.call_hierarchy_incoming_dirty
    });
    assert!(
        s.manager
            .call_hierarchy_incoming
            .iter()
            .any(|call| call.from.name == "caller")
    );
}

fn edits_for(edit: WorkspaceEdit, path: &Path) -> Vec<TextEdit> {
    if let Some(changes) = edit.changes {
        return changes
            .into_iter()
            .find(|(uri, _)| uri.to_file_path().ok().as_deref() == Some(path))
            .map(|(_, edits)| edits)
            .unwrap_or_default();
    }
    match edit.document_changes {
        Some(DocumentChanges::Edits(documents)) => documents
            .into_iter()
            .filter(|document| {
                document.text_document.uri.to_file_path().ok().as_deref() == Some(path)
            })
            .flat_map(|document| document.edits)
            .map(|edit| match edit {
                OneOf::Left(edit) => edit,
                OneOf::Right(edit) => edit.text_edit,
            })
            .collect(),
        Some(DocumentChanges::Operations(operations)) => operations
            .into_iter()
            .flat_map(|operation| match operation {
                DocumentChangeOperation::Edit(document)
                    if document.text_document.uri.to_file_path().ok().as_deref() == Some(path) =>
                {
                    document
                        .edits
                        .into_iter()
                        .map(|edit| match edit {
                            OneOf::Left(edit) => edit,
                            OneOf::Right(edit) => edit.text_edit,
                        })
                        .collect()
                }
                _ => Vec::new(),
            })
            .collect(),
        None => Vec::new(),
    }
}

fn unicode_workflow(language: &str) {
    let source = if language == "python" {
        "marker = '😀'; value = 42\nresult = value"
    } else {
        "/* 😀 */ int value = 42;\nint caller(void) { return value; }"
    };
    let mut s = Session::new(language, source);
    s.diagnostics(false);
    let (line, col) = point(source, "value =", 2);
    assert!(s.manager.request_definition(&s.path, line, col));
    wait(&mut s.manager, "Unicode definition", |m| m.definition_dirty);
    let location = &s.manager.definition_locations[0];
    let expected = point(source, "value =", 0);
    assert_eq!(
        (location.range.start.line, location.range.start.character),
        expected
    );
    assert!(
        s.manager
            .request_rename(&s.path, line, col, "renamed_value")
    );
    wait(&mut s.manager, "Unicode rename", |m| {
        m.rename_edit.is_some()
    });
    let edits = edits_for(s.manager.rename_edit.take().unwrap(), &s.path);
    assert_eq!(edits.len(), 2, "{:?}", s.manager.rename_edit);
    let mut editor = crate::editor::Editor::test_with_text(source);
    crate::app_lsp_edits::apply(&mut editor, &edits).unwrap();
    assert_eq!(
        editor.rope.to_string(),
        source.replace("value", "renamed_value")
    );
    editor.undo();
    assert_eq!(editor.rope.to_string(), source);
}

#[test]
#[ignore = "requires installed pyright-langserver"]
fn real_lsp_pyright_unicode_navigation_and_rename() {
    unicode_workflow("python");
}

#[test]
#[ignore = "requires installed clangd"]
fn real_lsp_clangd_c_unicode_navigation_and_rename() {
    unicode_workflow("c");
}

#[test]
#[ignore = "requires installed clangd"]
fn real_lsp_clangd_cpp_unicode_navigation_and_rename() {
    unicode_workflow("cpp");
}

#[test]
#[ignore = "requires installed pyright-langserver"]
fn real_lsp_pyright_cross_file_definitions_and_rename() {
    let helper = "def calculate_total(value: int) -> int:\n    return value * 2\n";
    let source = "from helper import calculate_total\nresult = calculate_total(21)\n";
    let mut s = Session::configured("python", source, None, &[("helper.py", helper)], 0);
    s.diagnostics(false);
    let (line, col) = point(source, "calculate_total(21)", 3);
    assert!(s.manager.request_definition(&s.path, line, col));
    wait(&mut s.manager, "imported definition", |m| {
        m.definition_dirty
    });
    assert_eq!(
        s.manager.definition_locations[0]
            .uri
            .to_file_path()
            .unwrap(),
        s.root.join("helper.py")
    );
    assert!(s.manager.request_rename(&s.path, line, col, "double_value"));
    wait(&mut s.manager, "cross-file rename", |m| {
        m.rename_edit.is_some()
    });
    let edit = s.manager.rename_edit.take().unwrap();
    assert_eq!(edits_for(edit.clone(), &s.path).len(), 2);
    assert_eq!(edits_for(edit, &s.root.join("helper.py")).len(), 1);
}

fn clangd_header_workflow(language: &str) {
    let source = "#include \"helper.h\"\nint caller(void) { return calculate_total(21); }\n";
    let mut s = Session::configured(
        language,
        source,
        None,
        &[("helper.h", "int calculate_total(int value);\n")],
        0,
    );
    s.diagnostics(false);
    let (line, col) = point(source, "calculate_total(21)", 3);
    assert!(s.manager.request_definition(&s.path, line, col));
    wait(&mut s.manager, "header definition", |m| m.definition_dirty);
    assert_eq!(
        s.manager.definition_locations[0]
            .uri
            .to_file_path()
            .unwrap(),
        s.root.join("helper.h")
    );
    assert!(s.manager.request_signature_help(&s.path, line, col + 13));
    wait(&mut s.manager, "header signature", |m| {
        m.signature_help.is_some()
    });
    assert!(
        s.manager.signature_help.as_ref().unwrap().signatures[0]
            .label
            .contains("value")
    );
}

#[test]
#[ignore = "requires installed clangd"]
fn real_lsp_clangd_c_header_navigation() {
    clangd_header_workflow("c");
}

#[test]
#[ignore = "requires installed clangd"]
fn real_lsp_clangd_cpp_header_navigation() {
    clangd_header_workflow("cpp");
}

#[test]
#[ignore = "requires installed pyright-langserver"]
fn real_lsp_pyright_startup_edits_configuration_and_restart() {
    let settings = serde_json::json!({"python":{"analysis":{"typeCheckingMode":"strict", "diagnosticSeverityOverrides":{"reportMissingTypeStubs":"none"}}}});
    let mut s = Session::configured("python", "x: int = 42\n", Some(settings.clone()), &[], 100);
    s.diagnostics(false);
    assert_eq!(
        s.manager.clients[0].configuration(Some("python.analysis")),
        settings["python"]["analysis"]
    );
    assert!(
        !s.manager.request_format_document(&s.path, 4, true),
        "Pyright does not format; unsupported commands must fail immediately"
    );
    let old_process = s.manager.clients[0].child.id();
    s.manager.shutdown_all();
    s.manager
        .sync_document_owned(&s.path, 200, "x: int = 'bad'\n".into());
    wait(&mut s.manager, "restart", |m| m.clients[0].initialized);
    assert_ne!(s.manager.clients[0].child.id(), old_process);
    s.diagnostics(true);
    s.manager.clients[0].shutdown().unwrap();
    assert!(
        s.manager.clients[0]
            .child
            .try_wait()
            .unwrap()
            .unwrap()
            .success(),
        "server must acknowledge shutdown before exit"
    );
}

#[test]
#[ignore = "requires installed clangd"]
fn real_lsp_clangd_startup_burst_and_formatting_round_trip() {
    let source = "/*😀*/ int main(){int value=42;return value;}";
    let mut s = Session::configured("cpp", source, None, &[], 100);
    s.diagnostics(false);
    assert!(s.manager.request_format_document(&s.path, 4, true));
    wait(&mut s.manager, "format response", |m| m.format_dirty);
    let mut editor = crate::editor::Editor::test_with_text(source);
    crate::app_lsp_edits::apply(&mut editor, &s.manager.format_edits).unwrap();
    assert!(editor.rope.to_string().contains("int value = 42;"));
    assert!(editor.rope.to_string().contains("/*😀*/"));
    s.change(editor.rope.to_string(), 200);
    s.manager.format_dirty = false;
    assert!(s.manager.request_format_document(&s.path, 4, true));
    wait(&mut s.manager, "idempotent formatting", |m| m.format_dirty);
    assert!(s.manager.format_edits.is_empty());
    editor.undo();
    assert_eq!(editor.rope.to_string(), source);
}

#[test]
#[ignore = "requires installed pyright-langserver"]
fn real_lsp_pyright_organize_imports_command_applies_server_edit() {
    let source = "import sys\nimport os\nprint(os.name, sys.version)\n";
    let mut s = Session::new("python", source);
    s.diagnostics(false);
    let command = lsp_types::Command {
        title: "Organize Imports".into(),
        command: "pyright.organizeimports".into(),
        arguments: Some(vec![serde_json::json!(
            Url::from_file_path(&s.path).unwrap()
        )]),
    };
    assert!(s.manager.execute_command(&s.path, &command));
    wait(&mut s.manager, "organize imports workspace edit", |m| {
        !m.workspace_edit_requests.is_empty()
    });
    let (process, id, edit) = s.manager.workspace_edit_requests.remove(0);
    let mut editor = crate::editor::Editor::test_with_text(source);
    let result = crate::app_lsp_edits::apply(&mut editor, &edits_for(edit, &s.path));
    s.manager.reply_workspace_edit(Some((process, id)), &result);
    result.unwrap();
    assert!(
        editor
            .rope
            .to_string()
            .starts_with("import os\nimport sys\n"),
        "{}",
        editor.rope
    );
    s.change(editor.rope.to_string(), 2);
    assert!(s.manager.request_hover(&s.path, 0, 8));
    wait(&mut s.manager, "imports hover", |m| {
        m.hover_content.is_some()
    });
}

#[test]
#[ignore = "requires installed pyright-langserver"]
fn real_lsp_pyright_auto_import_completion_preserves_snippet_and_undo() {
    let source = "# 😀\nCustomVal\n";
    let mut s = Session::configured(
        "python",
        source,
        Some(serde_json::json!({"python":{"analysis":{"autoImportCompletions":true}}})),
        &[("helper.py", "class CustomValue:\n    pass\n")],
        0,
    );
    s.diagnostics(true);
    let helper = s.root.join("helper.py");
    s.manager
        .sync_document_owned(&helper, 1, "class CustomValue:\n    pass\n".into());
    wait(&mut s.manager, "auto-import module analysis", |m| {
        m.get_diagnostics(&helper).is_some()
    });
    assert!(s.manager.request_completion(&s.path, 1, 9, "CustomVal"));
    wait(&mut s.manager, "auto-import completion", |m| {
        !m.completion_items.is_empty()
    });
    let index = s
        .manager
        .completion_items
        .iter()
        .position(|item| item.label == "CustomValue")
        .expect("workspace class offered for auto import");
    assert!(s.manager.request_completion_resolve(&s.path, index));
    wait(&mut s.manager, "auto-import resolve", |m| {
        m.completion_resolve_dirty
    });
    assert!(
        !s.manager.completion_items[index]
            .additional_text_edits
            .is_empty()
    );
    let mut editor = crate::editor::Editor::test_with_text(source);
    editor.set_cursor(1, 9);
    crate::app_lsp_edits::complete(&mut editor, &s.manager.completion_items[index]).unwrap();
    assert!(
        editor
            .rope
            .to_string()
            .contains("from helper import CustomValue"),
        "{}",
        editor.rope
    );
    assert_eq!(
        editor.rope.line(editor.cursor_line).to_string().trim(),
        "CustomValue"
    );
    editor.undo();
    assert_eq!(editor.rope.to_string(), source);
}

#[test]
#[ignore = "requires installed pyright-langserver"]
fn real_lsp_pyright_configuration_resolves_extra_import_paths() {
    let mut s = Session::configured(
        "python",
        "from model import answer\nvalue: int = answer\n",
        Some(serde_json::json!({"python":{"analysis":{"extraPaths":["./support"]}}})),
        &[("support/model.py", "answer: int = 42\n")],
        0,
    );
    s.diagnostics(false);
    assert!(s.manager.request_definition(&s.path, 1, 16));
    wait(&mut s.manager, "configured import path", |m| {
        m.definition_dirty
    });
    assert_eq!(
        s.manager.definition_locations[0]
            .uri
            .to_file_path()
            .unwrap(),
        s.root.join("support/model.py")
    );
}

fn quick_fix(language: &str) {
    let source = "int main(void) { return 0 }\n";
    let mut s = Session::new(language, source);
    s.diagnostics(true);
    assert!(s.manager.request_code_action(&CodeActionParams {
        path: &s.path,
        start_line: 0,
        start_col: 0,
        end_line: 0,
        end_col: 27,
        diagnostics: s.manager.code_action_diagnostics(&s.path, 0)
    }));
    wait(&mut s.manager, "missing semicolon quick fix", |m| {
        !m.code_actions.is_empty()
    });
    let edit = s
        .manager
        .code_actions
        .iter()
        .find_map(|action| action.edit.clone())
        .expect("clangd offered a fix-it edit");
    let mut editor = crate::editor::Editor::test_with_text(source);
    crate::app_lsp_edits::apply(&mut editor, &edits_for(edit, &s.path)).unwrap();
    assert!(editor.rope.to_string().contains("return 0;"));
    s.change(editor.rope.to_string(), 2);
    s.diagnostics(false);
}

#[test]
#[ignore = "requires installed clangd"]
fn real_lsp_clangd_c_diagnostic_quick_fix() {
    quick_fix("c");
}

#[test]
#[ignore = "requires installed clangd"]
fn real_lsp_clangd_cpp_diagnostic_quick_fix() {
    quick_fix("cpp");
}

fn compile_flags(language: &str) {
    let source = "#if !defined(PROJECT_FEATURE)\n#error missing project build flags\n#endif\nint configured_value = PROJECT_FEATURE;\n";
    let mut s = Session::configured(
        language,
        source,
        None,
        &[("compile_flags.txt", "-DPROJECT_FEATURE=42\n")],
        0,
    );
    s.diagnostics(false);
    assert!(s.manager.request_hover(&s.path, 3, 26));
    wait(&mut s.manager, "build macro hover", |m| {
        m.hover_content.is_some()
    });
    assert!(s.manager.hover_content.as_ref().unwrap().contains("42"));
}

#[test]
#[ignore = "requires installed clangd"]
fn real_lsp_clangd_c_project_compile_flags() {
    compile_flags("c");
}

#[test]
#[ignore = "requires installed clangd"]
fn real_lsp_clangd_cpp_project_compile_flags() {
    compile_flags("cpp");
}

#[test]
#[ignore = "requires installed pyright-langserver and clangd"]
fn real_lsp_mixed_python_c_cpp_routes_hierarchy_and_survives_completion_bursts() {
    let mut s = Session::new("python", PYTHON);
    s.diagnostics(true);
    s.manager.register_config(LspServerConfig {
        language: "cpp".into(),
        command: std::env::var("VTCODE_CLANGD").unwrap_or("clangd".into()),
        args: vec!["--background-index=false".into()],
        file_patterns: vec!["*.c".into(), "*.cpp".into()],
        init_options: None,
        settings: None,
    });
    let c = s.root.join("native.c");
    let cpp = s.root.join("native.cpp");
    std::fs::write(&c, C).unwrap();
    std::fs::write(&cpp, C).unwrap();
    s.manager.sync_document_owned(&c, 1, C.into());
    s.manager.sync_document_owned(&cpp, 1, C.into());
    wait(&mut s.manager, "both servers ready", |m| {
        m.clients.len() == 2 && m.clients.iter().all(|client| client.initialized)
    });
    wait(&mut s.manager, "both native documents", |m| {
        m.get_diagnostics(&c).is_some() && m.get_diagnostics(&cpp).is_some()
    });
    for _ in 0..30 {
        assert!(s.manager.request_completion(&s.path, 3, 11, "sum"));
        assert!(s.manager.request_completion(&cpp, 4, 20, "sum"));
    }
    wait(&mut s.manager, "latest C++ completion", |m| {
        !m.completion_items.is_empty()
    });
    assert!(
        s.manager
            .completion_items
            .iter()
            .any(|item| item.label.contains("sum_values"))
    );
    assert!(s.manager.request_prepare_call_hierarchy(&cpp, 0, 6));
    wait(
        &mut s.manager,
        "C++ call hierarchy in Python workspace",
        |m| m.call_hierarchy_dirty,
    );
    let item = s.manager.call_hierarchy_items[0].clone();
    assert!(s.manager.request_incoming_calls(&item));
    wait(&mut s.manager, "hierarchy routed to clangd", |m| {
        m.call_hierarchy_incoming_dirty
    });
    assert!(
        s.manager
            .call_hierarchy_incoming
            .iter()
            .any(|call| call.from.name == "caller")
    );
    assert!(s.manager.request_hover(&s.path, 3, 11));
    wait(&mut s.manager, "Python remains responsive", |m| {
        m.hover_content.is_some()
    });
    assert!(
        s.manager
            .hover_content
            .as_ref()
            .unwrap()
            .contains("sum_values")
    );
    assert!(
        s.manager.request_errors.is_empty(),
        "{:?}",
        s.manager.request_errors
    );
}

fn typed_member_completion(language: &str, source: &str, expected: &str) {
    let mut session = Session::new(language, source);
    session.diagnostics(language != "python");
    let mut app = crate::app::App::test_new();
    app.explorer = crate::explorer::FileExplorer::new_deferred(session.root.clone());
    app.lsp_manager =
        std::mem::replace(&mut session.manager, LspManager::new(session.root.clone()));
    let mut editor = crate::editor::Editor::test_with_text(source);
    editor.file_path = Some(session.path.clone());
    editor.content_version = 1;
    let line = editor.rope.len_lines() - 1;
    editor.set_cursor(line, editor.rope.line(line).len_chars());
    app.tabs = vec![crate::app::Tab::new_file(editor)];
    app.active_tab = 0;
    crate::input::handle_editor_key(
        &mut app,
        crossterm::event::KeyCode::Char('.'),
        crossterm::event::KeyModifiers::NONE,
    );
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        app.poll_background_results();
        crate::app_lsp_poll::poll_lsp(&mut app, &mut false);
        if app.lsp_completion_active
            && app
                .lsp_manager
                .completion_items
                .iter()
                .any(|item| item.label.contains(expected))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "typed member completion failed: {:?}",
            app.lsp_manager.completion_items
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(std::fs::read_to_string(&session.path).unwrap(), source);
    assert_eq!(
        app.lsp_manager.last_notified_version.get(&session.path),
        Some(&app.editor().unwrap().content_version)
    );
}

#[test]
#[ignore = "requires installed pyright-langserver"]
fn real_lsp_pyright_typed_dot_completion_uses_unsaved_text() {
    typed_member_completion("python", "import math\nmath", "sqrt");
}

#[test]
#[ignore = "requires installed clangd"]
fn real_lsp_clangd_cpp_typed_dot_completion_uses_unsaved_text() {
    typed_member_completion(
        "cpp",
        "struct Point { int x; int y; };\nint main() { Point point; point",
        "x",
    );
}

fn incremental_burst(language: &str) {
    let python = language == "python";
    let header = if python { "# 😀 café\r\nvalue: int = 1\r\nprint(value)\r\n" }
        else { "// 😀 café\r\nint value = 1;\r\nint main(void) { return value; }\r\n" };
    let source = format!("{header}{}", if python { "# filler\r\n" } else { "// filler\r\n" }.repeat(6_000));
    let mut session = Session::new(language, &source);
    session.diagnostics(false);
    let sync = &session.manager.clients[0].capabilities.as_ref().unwrap().text_document_sync;
    assert!(matches!(sync, Some(TextDocumentSyncCapability::Kind(kind)) if *kind == TextDocumentSyncKind::INCREMENTAL)
        || matches!(sync, Some(TextDocumentSyncCapability::Options(options)) if options.change == Some(TextDocumentSyncKind::INCREMENTAL)));
    let mut rope = Rope::from_str(&source);
    let start = Instant::now();
    for version in 2..=51 {
        // Repeated insertions after a non-BMP character exercise UTF-16 ranges
        // without copying the rest of the file. Remove them again to test undo.
        let at = if python { 3 } else { 4 };
        if version % 2 == 0 { rope.insert(at, "終"); } else { rope.remove(at..at+1); }
        session.manager.sync_document(&session.path, version, rope.clone());
    }
    let declaration = rope.line_to_char(1) + if python { 0 } else { 4 };
    rope.remove(declaration..declaration+5);
    rope.insert(declaration, "latest_value");
    let call = rope.line_to_char(2) + if python { 6 } else { 24 };
    rope.remove(call..call+5);
    rope.insert(call, "latest_value");
    session.manager.sync_document(&session.path, 52, rope.clone());
    session.manager.request_hover(&session.path, 1, if python { 2 } else { 6 });
    wait(&mut session.manager, "hover after 51 incremental changes", |m| m.hover_content.as_ref().is_some_and(|text| text.contains("latest_value")));
    println!("{language}: 51 incremental updates plus hover in {:?}", start.elapsed());
    session.diagnostics(false);
    assert_eq!(std::fs::read_to_string(&session.path).unwrap(), source);
    assert!(session.manager.request_errors.is_empty());
}

#[test]
#[ignore = "requires installed pyright-langserver"]
fn real_lsp_pyright_incremental_unicode_burst() { incremental_burst("python"); }

#[test]
#[ignore = "requires installed clangd"]
fn real_lsp_clangd_c_incremental_unicode_burst() { incremental_burst("c"); }

#[test]
#[ignore = "requires installed clangd"]
fn real_lsp_clangd_cpp_incremental_unicode_burst() { incremental_burst("cpp"); }
