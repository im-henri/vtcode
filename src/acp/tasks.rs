//! Normalize reported task lists without treating prose or file contents as plans.
use super::protocol as p;
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
    Unknown,
}

impl TaskStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::InProgress => "In progress",
            Self::Completed => "Completed",
            Self::Cancelled => "Cancelled",
            Self::Unknown => "Unknown status",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskEntry {
    pub content: String,
    pub status: TaskStatus,
    pub priority: Option<p::PlanEntryPriority>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskList {
    pub entries: Vec<TaskEntry>,
}

impl TaskList {
    pub fn from_plan(plan: &p::Plan) -> Self {
        Self {
            entries: plan
                .entries
                .iter()
                .map(|entry| TaskEntry {
                    content: entry.content.clone(),
                    status: match entry.status {
                        p::PlanEntryStatus::Pending => TaskStatus::Pending,
                        p::PlanEntryStatus::InProgress => TaskStatus::InProgress,
                        p::PlanEntryStatus::Completed => TaskStatus::Completed,
                        _ => TaskStatus::Unknown,
                    },
                    priority: Some(entry.priority.clone()),
                })
                .collect(),
        }
    }

    pub fn completed(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.status == TaskStatus::Completed)
            .count()
    }

    pub fn current(&self) -> usize {
        self.entries
            .iter()
            .rposition(|entry| entry.status == TaskStatus::InProgress)
            .or_else(|| {
                self.entries
                    .iter()
                    .position(|entry| entry.status == TaskStatus::Pending)
            })
            .unwrap_or(0)
    }
}

/// Some ACP agents expose TODO lists only through tool input/output. Accept a
/// complete, validated structured list, never infer tasks from ordinary text.
/// File/command tools remain file/command tools even if their data has TODOs.
pub fn tool_tasks(tool: &p::ToolCall) -> Option<TaskList> {
    if tool
        .raw_output
        .as_ref()
        .is_some_and(|value| value.get("isError").and_then(Value::as_bool) == Some(true))
    {
        return None;
    }
    let named = task_tool_name(&tool.title)
        || [tool.raw_input.as_ref(), tool.raw_output.as_ref()]
            .into_iter()
            .flatten()
            .any(|value| {
                ["tool_name", "toolName"]
                    .iter()
                    .filter_map(|key| value.get(key).and_then(Value::as_str))
                    .any(task_tool_name)
            });
    if !matches!(tool.kind, p::ToolKind::Think | p::ToolKind::Other)
        && !(named && tool.kind == p::ToolKind::Read)
    {
        return None;
    }
    let input = tool
        .raw_input
        .as_ref()
        .and_then(|value| extract(value, named, 0));
    let array = named || input.is_some();
    tool.raw_output
        .as_ref()
        .and_then(|value| extract(value, array, 0))
        .or_else(|| {
            tool.content.iter().find_map(|content| {
                if let p::ToolCallContent::Content(content) = content
                    && let p::ContentBlock::Text(text) = &content.content
                {
                    extract(&Value::String(text.text.clone()), array, 0)
                } else {
                    None
                }
            })
        })
        .or(input)
}

fn task_tool_name(title: &str) -> bool {
    // Providers may append a human-readable summary to the tool name.
    let name: String = title
        .split(':')
        .next()
        .unwrap_or(title)
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    matches!(
        name.as_str(),
        "todowrite" | "todoread" | "writetodos" | "updatetodos" | "updateplan" | "managetodolist"
    )
}

fn extract(value: &Value, allow_array: bool, depth: usize) -> Option<TaskList> {
    if depth > 6 {
        return None;
    }
    if let Some(text) = value.as_str() {
        if text.len() > 512 * 1024 {
            return None;
        }
        let decoded: Value = serde_json::from_str(text).ok()?;
        return extract(&decoded, allow_array, depth + 1);
    }
    if allow_array && let Some(entries) = value.as_array() {
        return parse_entries(entries);
    }
    let object = value.as_object()?;
    if object.get("isError").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    for key in ["todos", "todoList", "todo_list", "plan", "entries"] {
        if let Some(entries) = object.get(key).and_then(Value::as_array) {
            if key == "entries" && entries.is_empty() && !allow_array {
                return None;
            }
            return parse_entries(entries);
        }
    }
    for key in ["result", "output", "content", "text"] {
        if let Some(nested) = object.get(key) {
            if let Some(list) = extract(nested, allow_array, depth + 1) {
                return Some(list);
            }
            // MCP/ACP text-content envelopes may wrap the result in an array.
            if let Some(contents) = nested.as_array() {
                for content in contents {
                    if content.get("type").and_then(Value::as_str) == Some("text")
                        && let Some(list) = content
                            .get("text")
                            .and_then(|text| extract(text, allow_array, depth + 1))
                    {
                        return Some(list);
                    }
                }
            }
        }
    }
    None
}

fn parse_entries(entries: &[Value]) -> Option<TaskList> {
    let entries = entries
        .iter()
        .map(|entry| {
            let content = ["content", "step", "title", "description"]
                .iter()
                .find_map(|key| entry.get(key).and_then(Value::as_str))?;
            if content.trim().is_empty() {
                return None;
            }
            let status = match entry.get("status")?.as_str()? {
                "pending" | "not-started" | "TODO_STATUS_PENDING" => TaskStatus::Pending,
                "in_progress" | "in-progress" | "TODO_STATUS_IN_PROGRESS" => TaskStatus::InProgress,
                "completed" | "TODO_STATUS_COMPLETED" => TaskStatus::Completed,
                "cancelled" | "canceled" | "TODO_STATUS_CANCELLED" | "TODO_STATUS_CANCELED" => {
                    TaskStatus::Cancelled
                }
                _ => return None,
            };
            let priority = match entry.get("priority") {
                None => None,
                Some(Value::String(value)) if value == "high" => Some(p::PlanEntryPriority::High),
                Some(Value::String(value)) if value == "medium" => {
                    Some(p::PlanEntryPriority::Medium)
                }
                Some(Value::String(value)) if value == "low" => Some(p::PlanEntryPriority::Low),
                _ => return None,
            };
            Some(TaskEntry {
                content: content.into(),
                status,
                priority,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(TaskList { entries })
}

#[cfg(test)]
#[path = "tasks_tests.rs"]
mod tests;
