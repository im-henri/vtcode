//! Render agent-owned command tools using their structured output, not JSON.
use super::tools::{decoded, string};
use super::*;
use serde_json::Value;

pub(super) struct ReportedTerminal {
    pub id: String,
    pub command: String,
    pub command_reported: bool,
    pub cwd: Option<std::path::PathBuf>,
    pub lines: Vec<String>,
    pub styled: Vec<vtcode::acp::terminal_style::TerminalLine>,
    pub columns: usize,
    pub status: String,
    pub running: bool,
    pub revision: u64,
}

fn output(value: &Value) -> Option<(String, Option<i64>)> {
    let original_text = value.as_str();
    let value = decoded(value);
    if let Some(text) = value.as_str() {
        return Some((text.into(), None));
    }
    let code = ["exitCode", "exit_code"]
        .iter()
        .find_map(|key| value.get(key).and_then(Value::as_i64));
    let stdout = string(&value, &["stdout"]);
    let stderr = string(&value, &["stderr"]);
    if stdout.is_some() || stderr.is_some() {
        let mut text = stdout.unwrap_or_default().to_owned();
        if let Some(stderr) = stderr.filter(|s| !s.is_empty()) {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str("[stderr]\n");
            text.push_str(stderr);
        }
        return Some((text, code));
    }
    // Common tool envelopes; don't guess at unrelated object fields.
    for key in ["result", "output"] {
        if let Some(inner) = value.get(key).filter(|v| v.is_object())
            && let Some((text, nested_code)) = output(inner)
        {
            return Some((text, code.or(nested_code)));
        }
    }
    tools::output_text(&value)
        .map(|text| (text, code))
        .or_else(|| code.map(|code| (String::new(), Some(code))))
        .or_else(|| original_text.map(|text| (text.into(), None)))
}

pub(super) fn parse(tool: &p::ToolCall, revision: u64) -> Option<Arc<ReportedTerminal>> {
    if tool
        .content
        .iter()
        .any(|c| !matches!(c, p::ToolCallContent::Content(_)))
    {
        // Keep hosted terminal links, diffs, and future content variants usable.
        return None;
    }
    let input = tool.raw_input.as_ref().map(decoded);
    let input = input.as_deref();
    let command = input
        .and_then(|v| string(v, &["command", "cmd"]))
        .map(str::to_owned)
        .or_else(|| {
            input?.get("commands")?.as_array().map(|commands| {
                commands
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        })
        .filter(|command| !command.trim().is_empty());
    let raw = tool.raw_output.as_ref().and_then(output);
    if tool.kind != p::ToolKind::Execute && (tool.kind != p::ToolKind::Other || command.is_none()) {
        return None;
    }
    // Terminal identity depends on the operation, never on a provider's final
    // output envelope. Completion must not turn a terminal into Markdown.
    let command_reported = command.is_some();
    let mut command = command.unwrap_or_else(|| tool.title.clone());
    if let Some(args) = input.and_then(|v| v.get("args")).and_then(Value::as_array) {
        for arg in args.iter().filter_map(Value::as_str) {
            command.push(' ');
            command.push_str(&serde_json::to_string(arg).unwrap_or_default());
        }
    }
    let (text, mut code) = raw.unwrap_or_else(|| {
        (
            tool.raw_output
                .as_ref()
                .map(tools::readable)
                .unwrap_or_default(),
            None,
        )
    });
    let mut blocks = if text.is_empty() {
        Vec::new()
    } else {
        vec![text.clone()]
    };
    // Human-readable diagnostics (such as a permission notice) remain visible.
    for content in &tool.content {
        if let p::ToolCallContent::Content(content) = content {
            let content = content_text(&content.content);
            let content = serde_json::from_str::<Value>(&content)
                .ok()
                .and_then(|v| output(&v))
                .map_or(content, |(s, _)| s);
            if !content.trim().is_empty()
                && !clean_terminal(&text).contains(clean_terminal(&content).trim())
                && !blocks.iter().any(|block| block.trim() == content.trim())
            {
                blocks.push(content);
            }
        }
    }
    let mut raw_text = blocks.join("\n");
    let text = clean_terminal(&raw_text);
    // Copilot appends this process metadata to its literal output.
    if let Some(start) = text.rfind("<shellId: ")
        && let Some(metadata) = text[start..].trim().strip_suffix('>')
        && let Some((_, exit)) = metadata.split_once(" completed with exit code ")
        && let Ok(exit) = exit.parse::<i64>()
    {
        code = code.or(Some(exit));
        if let Some(start) = raw_text.rfind("<shellId: ") { raw_text.truncate(start); }
    }
    let styled = vtcode::acp::terminal_style::parse(&raw_text);
    let lines: Vec<String> = styled.iter().map(|line| line.text.clone()).collect();
    let columns = lines.iter().map(|s| s.width()).max().unwrap_or(0);
    let running = matches!(
        tool.status,
        p::ToolCallStatus::Pending | p::ToolCallStatus::InProgress
    );
    let status = if running {
        "● Running · Agent-managed".into()
    } else if let Some(code) = code {
        format!("Exit {code} · Read-only")
    } else if tool.status == p::ToolCallStatus::Failed {
        "! Failed · Read-only".into()
    } else {
        "✓ Completed · Read-only".into()
    };
    Some(Arc::new(ReportedTerminal {
        id: format!("tool:{}", tool.tool_call_id),
        command,
        command_reported,
        cwd: input
            .and_then(|v| string(v, &["cwd", "workingDirectory", "working_directory"]))
            .map(Into::into),
        lines,
        styled,
        columns,
        status,
        running,
        revision,
    }))
}
