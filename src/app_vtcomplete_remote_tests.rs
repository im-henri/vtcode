use std::io::{self, Write};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use crate::{
    app::{App, Tab},
    editor::Editor,
    remote::{self, workspace::Workspace},
};

struct TrackedWriter {
    pipe: io::PipeWriter,
    written: Arc<AtomicUsize>,
}

impl Write for TrackedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self.pipe.write(bytes)?;
        self.written.fetch_add(count, Ordering::Relaxed);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.pipe.flush()
    }
}

#[test]
fn remote_completion_reads_the_active_buffer_without_ssh_requests() {
    let directory = tempfile::tempdir().unwrap();
    for (name, text) in [
        ("active.txt", "remoteActiveWord\nremote"),
        ("inactive.txt", "remoteInactiveWord"),
        ("unopened.txt", "remoteUnopenedWord"),
    ] {
        std::fs::write(directory.path().join(name), text).unwrap();
    }
    let sent = Arc::new(AtomicUsize::new(0));
    let (server_read, client_write) = io::pipe().unwrap();
    let (client_read, server_write) = io::pipe().unwrap();
    let root = directory.path().to_owned();
    let server =
        std::thread::spawn(move || remote::service::serve(&root, server_read, server_write));
    let client = remote::client::Client::connect(
        client_read,
        TrackedWriter {
            pipe: client_write,
            written: sent.clone(),
        },
    )
    .unwrap();
    let workspace = Workspace::new(client);
    let mut app = App::new_deferred(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    for name in ["active.txt", "inactive.txt"] {
        let path = workspace.target_key(name).unwrap();
        let loaded = workspace.load(&path).unwrap();
        let mut editor = Editor::test_with_text(&loaded.rope.to_string());
        editor.file_path = Some(path);
        app.tabs.push(Tab::new_file(editor));
    }
    app.active_tab = 0;
    app.editor_mut().unwrap().set_cursor(1, 6);
    workspace.client.call(remote::Command::Ping).unwrap();
    let before = sent.load(Ordering::Relaxed);
    app.update_vtcomplete_explicit();
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.vtcomplete_cancel_token.is_none()
    });
    assert_eq!(app.vtcomplete_suggestions, ["remoteActiveWord"]);
    assert_eq!(sent.load(Ordering::Relaxed), before);
    workspace.client.call(remote::Command::Shutdown).unwrap();
    server.join().unwrap().unwrap();
}
