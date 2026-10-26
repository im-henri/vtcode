use super::*;

#[test]
fn bare_file_launch_keeps_a_usable_agent_workspace_for_existing_and_new_files() {
    for file in ["main.rs", "new-file.rs"] {
        let directory = startup_file_directory(Path::new(file));
        assert!(!directory.as_os_str().is_empty());
        let workspace = vtcode::acp::Workspace::new(&directory).unwrap();
        assert_eq!(
            workspace.root(),
            std::env::current_dir().unwrap().canonicalize().unwrap()
        );
    }
    assert_eq!(
        startup_file_directory(Path::new("src/main.rs")),
        Path::new("src")
    );
}
