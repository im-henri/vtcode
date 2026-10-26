use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Deserialize;
use walkdir::WalkDir;

pub(crate) const MANIFEST_FILE_NAME: &str = "extension.json";
pub(crate) const SUPPORTED_API_VERSION: &str = "2";

pub(crate) fn supports_api(version: &str) -> bool {
    matches!(version, "1" | SUPPORTED_API_VERSION)
}

pub(crate) fn default_enabled() -> bool {
    true
}

pub(crate) fn default_api_version() -> String {
    "1".to_string()
}

pub(crate) fn parse_file_open_extensions(spec: &str) -> Vec<String> {
    spec.split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .map(|part| part.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|part| !part.is_empty())
        .collect()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExtensionManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    /// GitHub repository containing the extension's source code.
    #[serde(default)]
    pub repository: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub main: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_api_version")]
    pub api_version: String,
    /// Optional LSP server configuration for language support
    #[serde(default)]
    pub lsp: Option<crate::lsp::LspServerConfig>,
    /// Optional bundle configuration for self-contained extensions
    #[serde(default)]
    pub bundle: Option<BundleConfig>,
}

// ── Bundle configuration ────────────────────────────────────────────

/// Bundle configuration allowing extensions to include platform-specific binaries.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BundleConfig {
    /// Platform-specific binary mappings. Key is "{os}-{arch}" (e.g. "windows-x86_64").
    #[serde(default)]
    pub platforms: HashMap<String, PlatformBundle>,
}

/// Per-platform bundle: paths to binaries relative to the extension directory.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlatformBundle {
    /// Path to the main command binary (relative to extension dir).
    /// Overrides the extension's configured command.
    #[serde(default)]
    pub command: Option<String>,
    /// Path to a bundled Node.js binary (relative to the extension directory).
    #[serde(default)]
    pub node_command: Option<String>,
    /// Additional arguments to prepend (e.g. for node-based servers the JS entry point).
    #[serde(default)]
    pub prepend_args: Vec<String>,
}

/// Returns the current platform string: "{os}-{arch}".
pub(crate) fn current_platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" | "x86" => "x86_64",
        "aarch64" => "aarch64",
        other => other,
    };
    format!("{os}-{arch}")
}

/// Resolve a command through bundle config. Returns the absolute path if a bundle
/// override exists for the current platform, otherwise returns None.
pub(crate) fn resolve_bundled_command(
    extension_dir: &Path,
    bundle: Option<&BundleConfig>,
    field: BundleField,
) -> Option<PathBuf> {
    let bundle = bundle?;
    let platform = current_platform();
    let platform_bundle = bundle.platforms.get(&platform)?;
    let relative = match field {
        BundleField::Command => platform_bundle.command.as_deref()?,
        BundleField::NodeCommand => platform_bundle.node_command.as_deref()?,
    };
    let resolved = extension_dir.join(relative);
    // Security: ensure resolved path stays within extension directory
    if !resolved.starts_with(extension_dir) {
        return None;
    }
    Some(resolved)
}

/// Which field to resolve from the bundle config.
#[derive(Debug, Clone, Copy)]
pub(crate) enum BundleField {
    /// The extension's main command.
    Command,
    /// A bundled Node.js command.
    NodeCommand,
}

/// Get prepend_args from the platform bundle for the current platform.
pub(crate) fn bundled_prepend_args(bundle: Option<&BundleConfig>) -> Vec<String> {
    let bundle = match bundle {
        Some(b) => b,
        None => return Vec::new(),
    };
    let platform = current_platform();
    match bundle.platforms.get(&platform) {
        Some(pb) => pb.prepend_args.clone(),
        None => Vec::new(),
    }
}

/// Ensure a file has executable permission on Unix. No-op on Windows.
#[allow(unused_variables)]
pub(crate) fn ensure_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path)
            .map_err(|e| format!("Cannot read metadata for {}: {e}", path.display()))?;
        let mut perms = meta.permissions();
        let mode = perms.mode();
        if mode & 0o111 == 0 {
            perms.set_mode(mode | 0o755);
            std::fs::set_permissions(path, perms).map_err(|e| {
                format!(
                    "Cannot set executable permission on {}: {e}",
                    path.display()
                )
            })?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct ExtensionCommand {
    pub id: String,
    pub title: String,
    pub runtime_index: i32,
}

#[derive(Debug, Clone)]
pub(crate) struct ExtensionView {
    pub id: String,
    pub title: String,
    pub content: String,
    pub key_handler: Option<String>,
    pub mouse_handler: Option<String>,
    pub scroll_handler: Option<String>,
    pub resize_handler: Option<String>,
    pub close_handler: Option<String>,
    pub hover_handler: Option<String>,
    pub context_menu_handler: Option<String>,
    pub pointer_capture: bool,
    pub input_pending: bool,
    /// Published surfaces are immutable; input, tab snapshots and drawing share
    /// them without copying every span in every open custom editor.
    pub frame: Option<std::sync::Arc<crate::extension_surface::ViewFrame>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExtensionOverlay {
    pub id: String,
    pub title: String,
    pub content: String,
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub mouse_handler: Option<String>,
    pub key_handler: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ExtensionTickHandler {
    pub export_name: String,
    pub interval_ms: u32,
    pub background: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ExtensionContextCommand {
    pub extension_id: String,
    pub command_id: String,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExtensionState {
    Loaded,
    Unloaded,
    Error,
}

impl ExtensionState {
    pub(crate) fn label(self) -> &'static str {
        match self {
            ExtensionState::Loaded => "Loaded",
            ExtensionState::Unloaded => "Unloaded",
            ExtensionState::Error => "Error",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ExtensionDiagnostic {
    pub file: String,
    pub line: usize,
    pub col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub severity: u8,
    pub message: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ExtensionRecordView {
    pub id: String,
    pub name: String,
    pub version: String,
    pub repository: String,
    pub description: String,
    pub state: ExtensionState,
    pub status_text: String,
    pub commands: Vec<ExtensionCommand>,
    pub views: HashMap<String, ExtensionView>,
    pub _overlays: HashMap<String, ExtensionOverlay>,
    pub last_error: Option<String>,
    pub logs: Vec<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct EditorContext {
    pub remote_workspace: Option<crate::remote::workspace::Workspace>,
    pub active_file_path: Option<String>,
    pub active_buffer: Option<crate::extension_buffer::Buffer>,
    pub active_modified: bool,
    pub modified_file_paths: Vec<PathBuf>,
    pub cursor_line: Option<usize>,
    pub cursor_col: Option<usize>,
    pub selection_range: Option<(usize, usize, usize, usize)>,
    pub workspace_root: Option<String>,
}

/// Invocation metadata and a lazy immutable buffer. Cloning or preparing an
/// invocation never flattens the active Rope; only a guest byte read does.
#[derive(Clone, Default)]
pub(crate) struct EditorContextSnapshot {
    pub context: EditorContext,
}

impl EditorContextSnapshot {
    pub(crate) fn into_context(self) -> EditorContext {
        self.context
    }
}

#[derive(Debug, Clone)]
pub(crate) enum TextEditRequest {
    /// Open generated text in a new unsaved buffer; the user chooses its path.
    OpenDocument { name: String, text: String },
    InsertText {
        line: usize,
        col: usize,
        text: String,
    },
    DeleteRange {
        start_line: usize,
        start_col: usize,
        end_line: usize,
        end_col: usize,
    },
    ReplaceSelection {
        text: String,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct DiscoveredExtension {
    pub(crate) manifest: ExtensionManifest,
    pub(crate) manifest_path: PathBuf,
    pub(crate) wasm_path: PathBuf,
    pub(crate) manifest_mtime: Option<SystemTime>,
    pub(crate) wasm_mtime: Option<SystemTime>,
}

/// Scan extension search roots and return discovered extensions with notices.
pub(crate) fn discover_extensions(
    search_roots: &[PathBuf],
) -> (Vec<DiscoveredExtension>, Vec<String>) {
    let mut discovered = Vec::new();
    let mut notices = Vec::new();
    let mut seen_ids = HashSet::new();

    for root in search_roots {
        if !root.exists() {
            continue;
        }
        for entry in WalkDir::new(root)
            .max_depth(2)
            .into_iter()
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() || entry.file_name() != MANIFEST_FILE_NAME {
                continue;
            }
            let manifest_path = entry.path().to_path_buf();
            let manifest_json = match std::fs::read_to_string(&manifest_path) {
                Ok(content) => content,
                Err(err) => {
                    notices.push(format!(
                        "Failed reading extension manifest {}: {err}",
                        manifest_path.display()
                    ));
                    continue;
                }
            };
            let manifest = match serde_json::from_str::<ExtensionManifest>(&manifest_json) {
                Ok(manifest) => manifest,
                Err(err) => {
                    notices.push(format!(
                        "Invalid extension manifest {}: {err}",
                        manifest_path.display()
                    ));
                    continue;
                }
            };
            if manifest.id.trim().is_empty() {
                notices.push(format!(
                    "Extension manifest {} is missing a non-empty id",
                    manifest_path.display()
                ));
                continue;
            }
            if let Err(error) = crate::extension_install::validate_manifest(&manifest) {
                notices.push(format!("Extension {}: {error}", manifest.name));
                continue;
            }
            if !seen_ids.insert(manifest.id.clone()) {
                notices.push(format!(
                    "Duplicate extension id detected and skipped: {}",
                    manifest.id
                ));
                continue;
            }
            let extension_dir = manifest_path
                .parent()
                .map_or_else(|| root.clone(), Path::to_path_buf);
            let wasm_path = extension_dir.join(&manifest.main);
            // Prevent path traversal: WASM must be inside extension directory
            if !wasm_path.starts_with(&extension_dir) {
                notices.push(format!(
                    "Extension {}: WASM path traverses outside extension directory",
                    manifest.name
                ));
                continue;
            }
            let manifest_mtime = std::fs::metadata(&manifest_path)
                .ok()
                .and_then(|meta| meta.modified().ok());
            let wasm_mtime = std::fs::metadata(&wasm_path)
                .ok()
                .and_then(|meta| meta.modified().ok());
            discovered.push(DiscoveredExtension {
                manifest,
                manifest_path,
                wasm_path,
                manifest_mtime,
                wasm_mtime,
            });
        }
    }

    discovered.sort_by_cached_key(|r| r.manifest.name.to_lowercase());
    (discovered, notices)
}

/// Check whether a discovered extension differs from its existing record.
pub(crate) fn record_changed(
    existing_manifest_path: &Path,
    existing_wasm_path: &Path,
    existing_manifest: &ExtensionManifest,
    existing_manifest_mtime: Option<SystemTime>,
    existing_wasm_mtime: Option<SystemTime>,
    discovered: &DiscoveredExtension,
) -> bool {
    existing_manifest_path != discovered.manifest_path
        || existing_wasm_path != discovered.wasm_path
        || existing_manifest.main != discovered.manifest.main
        || existing_manifest.version != discovered.manifest.version
        || existing_manifest.description != discovered.manifest.description
        || existing_manifest.repository != discovered.manifest.repository
        || existing_manifest.enabled != discovered.manifest.enabled
        || existing_manifest.api_version != discovered.manifest.api_version
        || existing_manifest_mtime != discovered.manifest_mtime
        || existing_wasm_mtime != discovered.wasm_mtime
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_file_open_extensions ───────────────────────────────────
    #[test]
    fn file_open_extension_lists_normalize_dots_case_and_separators() {
        let cases: &[(&str, &[&str])] = &[
            ("", &[]),
            ("rs", &["rs"]),
            (".rs", &["rs"]),
            ("rs,toml,json", &["rs", "toml", "json"]),
            ("py;js;ts", &["py", "js", "ts"]),
            ("c cpp h", &["c", "cpp", "h"]),
            (".rs, .toml; json  xml", &["rs", "toml", "json", "xml"]),
            ("RS,.TOML", &["rs", "toml"]),
            ("  .py , .js  ", &["py", "js"]),
        ];
        for &(input, expected) in cases {
            assert_eq!(parse_file_open_extensions(input), expected, "{input:?}");
        }
    }

    // ── ExtensionManifest JSON deserialization ───────────────────────
    fn minimal_manifest_json() -> &'static str {
        r#"{"id":"test-ext","name":"Test","version":"0.1.0"}"#
    }

    #[test]
    fn deserialize_minimal_manifest() {
        let m: ExtensionManifest = serde_json::from_str(minimal_manifest_json()).unwrap();
        assert_eq!(m.id, "test-ext");
        assert_eq!(m.name, "Test");
        assert_eq!(m.version, "0.1.0");
        assert!(m.description.is_empty());
        assert!(m.main.is_empty());
        assert!(m.enabled); // default
        assert_eq!(m.api_version, "1"); // default
        assert!(m.lsp.is_none());
    }

    #[test]
    fn deserialize_full_manifest() {
        let json = r#"{
            "id": "hello",
            "name": "Hello World",
            "version": "1.0.0",
            "description": "A test extension",
            "main": "hello.wasm",
            "enabled": false,
            "apiVersion": "2"
        }"#;
        let m: ExtensionManifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.id, "hello");
        assert_eq!(m.main, "hello.wasm");
        assert!(!m.enabled);
        assert_eq!(m.api_version, "2");
        assert_eq!(m.description, "A test extension");
    }

    #[test]
    fn deserialize_missing_required_field_fails() {
        let json = r#"{"id":"test","version":"0.1.0"}"#; // missing "name"
        assert!(serde_json::from_str::<ExtensionManifest>(json).is_err());
    }

    // ── record_changed ──────────────────────────────────────────────
    fn make_manifest(
        version: &str,
        desc: &str,
        main: &str,
        enabled: bool,
        api: &str,
    ) -> ExtensionManifest {
        ExtensionManifest {
            id: "test".into(),
            name: "Test".into(),
            version: version.into(),
            repository: "https://github.com/im-henri/vtcode".into(),
            description: desc.into(),
            main: main.into(),
            enabled,
            api_version: api.into(),
            lsp: None,
            bundle: None,
        }
    }

    fn make_discovered(
        manifest: ExtensionManifest,
        manifest_path: &str,
        wasm_path: &str,
    ) -> DiscoveredExtension {
        DiscoveredExtension {
            manifest,
            manifest_path: PathBuf::from(manifest_path),
            wasm_path: PathBuf::from(wasm_path),
            manifest_mtime: None,
            wasm_mtime: None,
        }
    }

    #[test]
    fn record_change_detection_tracks_manifest_identity_and_disk_updates() {
        let manifest = make_manifest("1.0.0", "desc", "main.wasm", true, "1");
        let original = make_discovered(manifest.clone(), "/ext/extension.json", "/ext/main.wasm");
        let changed = |discovered: &DiscoveredExtension| {
            record_changed(
                Path::new("/ext/extension.json"),
                Path::new("/ext/main.wasm"),
                &manifest,
                None,
                None,
                discovered,
            )
        };
        assert!(!changed(&original));
        type Change = fn(&mut DiscoveredExtension);
        let cases: &[(&str, Change)] = &[
            ("version", |entry| entry.manifest.version = "2.0.0".into()),
            ("manifest path", |entry| {
                entry.manifest_path = "/other/extension.json".into()
            }),
            ("Wasm path", |entry| {
                entry.wasm_path = "/other/main.wasm".into()
            }),
            ("enabled", |entry| entry.manifest.enabled = false),
            ("mtime", |entry| {
                entry.manifest_mtime = Some(SystemTime::UNIX_EPOCH)
            }),
            ("description", |entry| {
                entry.manifest.description = "new desc".into()
            }),
            ("API version", |entry| {
                entry.manifest.api_version = "2".into()
            }),
        ];
        for &(label, change) in cases {
            let mut discovered = original.clone();
            change(&mut discovered);
            assert!(changed(&discovered), "{label}");
        }
    }

    // ── discover_extensions ─────────────────────────────────────────
    #[test]
    fn discover_extensions_empty_roots() {
        let (exts, notices) = discover_extensions(&[]);
        assert!(exts.is_empty());
        assert!(notices.is_empty());
    }

    #[test]
    fn discover_extensions_nonexistent_root_skipped() {
        let (exts, notices) =
            discover_extensions(&[PathBuf::from("/nonexistent/path/that/does/not/exist")]);
        assert!(exts.is_empty());
        assert!(notices.is_empty());
    }

    #[test]
    fn discover_extensions_with_temp_dir() {
        let temporary = tempfile::tempdir().unwrap();
        let dir = temporary.path().to_path_buf();
        let ext_dir = dir.join("my-ext");
        let _ = std::fs::create_dir_all(&ext_dir);
        std::fs::write(
            ext_dir.join("extension.json"),
            r#"{"id":"my-ext","name":"My Ext","version":"0.1.0","main":"ext.wasm","repository":"https://github.com/im-henri/vtcode"}"#,
        )
        .unwrap();
        std::fs::write(ext_dir.join("ext.wasm"), b"fake wasm").unwrap();

        let (exts, notices) = discover_extensions(std::slice::from_ref(&dir));
        assert_eq!(exts.len(), 1);
        assert_eq!(exts[0].manifest.id, "my-ext");
        assert!(notices.is_empty());
    }

    #[test]
    fn discover_extensions_invalid_json_produces_notice() {
        let temporary = tempfile::tempdir().unwrap();
        let dir = temporary.path().to_path_buf();
        let ext_dir = dir.join("bad-ext");
        let _ = std::fs::create_dir_all(&ext_dir);
        std::fs::write(ext_dir.join("extension.json"), "not valid json").unwrap();

        let (exts, notices) = discover_extensions(std::slice::from_ref(&dir));
        assert!(exts.is_empty());
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("Invalid extension manifest"));
    }

    #[test]
    fn discover_extensions_empty_id_produces_notice() {
        let temporary = tempfile::tempdir().unwrap();
        let dir = temporary.path().to_path_buf();
        let ext_dir = dir.join("no-id-ext");
        let _ = std::fs::create_dir_all(&ext_dir);
        std::fs::write(
            ext_dir.join("extension.json"),
            r#"{"id":"","name":"No ID","version":"0.1.0"}"#,
        )
        .unwrap();

        let (exts, notices) = discover_extensions(std::slice::from_ref(&dir));
        assert!(exts.is_empty());
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("missing a non-empty id"));
    }

    #[test]
    fn discover_extensions_duplicate_id_skipped() {
        let temporary = tempfile::tempdir().unwrap();
        let dir = temporary.path().to_path_buf();
        let ext1 = dir.join("ext1");
        let ext2 = dir.join("ext2");
        let _ = std::fs::create_dir_all(&ext1);
        let _ = std::fs::create_dir_all(&ext2);
        let json = r#"{"id":"same-id","name":"Ext","version":"0.1.0","main":"e.wasm","repository":"https://github.com/im-henri/vtcode"}"#;
        std::fs::write(ext1.join("extension.json"), json).unwrap();
        std::fs::write(ext2.join("extension.json"), json).unwrap();

        let (exts, notices) = discover_extensions(std::slice::from_ref(&dir));
        assert_eq!(exts.len(), 1);
        assert!(notices.iter().any(|n| n.contains("Duplicate extension id")));
    }
}
