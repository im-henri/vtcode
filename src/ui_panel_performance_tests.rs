use super::*;
use crate::lsp::LspDiagnostic;
use ratatui::{Terminal, backend::TestBackend};
use std::{path::PathBuf, time::Instant};

#[test]
#[ignore = "manual performance measurement"]
fn benchmark_large_problems_panel() {
    let mut app = App::test_new();
    for file in 0..100 {
        app.lsp_manager.diagnostics.insert(
            PathBuf::from(format!("src/file-{file}.rs")),
            (0..100)
                .rev()
                .map(|line| LspDiagnostic {
                    line,
                    col: 0,
                    end_line: line,
                    end_col: 1,
                    severity: if line % 2 == 0 {
                        lsp_types::DiagnosticSeverity::ERROR
                    } else {
                        lsp_types::DiagnosticSeverity::WARNING
                    },
                    message: format!("Diagnostic on line {line}"),
                })
                .collect(),
        );
    }
    app.lsp_manager.diagnostics_version += 1;
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    let mut samples = Vec::new();
    for run in 0..25 {
        let started = Instant::now();
        terminal
            .draw(|f| draw_panel_problems(f, &mut app, Rect::new(0, 0, 120, 24)))
            .unwrap();
        if run >= 4 {
            samples.push(started.elapsed());
        }
        std::hint::black_box(terminal.backend().buffer());
    }
    samples.sort();
    eprintln!(
        "problems_panel diagnostics=10000 median={:?}",
        samples[samples.len() / 2]
    );
}
