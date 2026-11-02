//! Presentation model for the embedded agent conversation.
//!
//! The daemon sends projected timeline items; this module turns them into the rows the desktop
//! client draws and derives the values the app derives at render time (turn timing, tool labels,
//! todo activity). It holds no GPUI state, so every rule here is unit tested.
use chrono::{DateTime, FixedOffset, Local};
use paseo_desktop::history::Entry;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const BLOCK_GAP: f32 = 16.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    User,
    Assistant,
    Tool,
    Reasoning,
    Todo,
    Notification,
    Compaction,
    Unsupported,
}

impl Kind {
    fn from_item(item: &Value) -> Self {
        match item["type"].as_str() {
            Some("user_message") => Self::User,
            Some("assistant_message") => Self::Assistant,
            Some("tool_call") => Self::Tool,
            Some("reasoning") => Self::Reasoning,
            Some("todo") => Self::Todo,
            Some("notification" | "error") => Self::Notification,
            Some("compaction") => Self::Compaction,
            _ => Self::Unsupported,
        }
    }
    fn is_tool_sequence(self) -> bool {
        matches!(self, Self::Tool | Self::Reasoning | Self::Todo)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Activity {
    Created(usize),
    Added(String),
    Started(String),
    Completed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub id: Option<String>,
    pub text: String,
    pub completed: bool,
    pub status: Option<String>,
    pub active_form: Option<String>,
}

impl Task {
    pub fn label(&self) -> &str {
        if !self.is_complete()
            && self.status.as_deref() == Some("in_progress")
            && let Some(active) = self.active_form.as_deref().filter(|text| !text.is_empty())
        {
            return active;
        }
        &self.text
    }
    pub fn is_complete(&self) -> bool {
        self.completed || self.status.as_deref() == Some("completed")
    }
    pub fn is_running(&self) -> bool {
        !self.is_complete() && self.status.as_deref() == Some("in_progress")
    }
    fn state(&self) -> &'static str {
        if self.is_complete() {
            "completed"
        } else if self.is_running() {
            "in_progress"
        } else {
            "pending"
        }
    }
}

pub fn tasks(item: &Value) -> Vec<Task> {
    item["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|task| {
                    Some(Task {
                        id: task["id"].as_str().map(str::to_owned),
                        text: task["text"].as_str()?.to_owned(),
                        completed: task["completed"] == true,
                        status: task["status"].as_str().map(str::to_owned),
                        active_form: task["activeForm"].as_str().map(str::to_owned),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn task_key(task: &Task, index: usize) -> String {
    task.id
        .clone()
        .unwrap_or_else(|| format!("{index}:{}", task.text))
}

/// The change this todo row reports compared with the previous todo row, as the app derives it
/// while reducing the stream.
fn activity(previous: Option<&[Task]>, current: &[Task]) -> Option<Activity> {
    if current.is_empty() {
        return None;
    }
    let Some(previous) = previous.filter(|previous| !previous.is_empty()) else {
        return Some(Activity::Created(current.len()));
    };
    let by_key = previous
        .iter()
        .enumerate()
        .map(|(index, task)| (task_key(task, index), task))
        .collect::<BTreeMap<_, _>>();
    for (index, task) in current.iter().enumerate() {
        let key = task_key(task, index);
        let Some(before) = by_key.get(&key) else {
            return Some(Activity::Added(task.text.clone()));
        };
        if before.state() == task.state() {
            continue;
        }
        return Some(if task.is_complete() {
            Activity::Completed(task.text.clone())
        } else if task.is_running() {
            Activity::Started(task.text.clone())
        } else {
            Activity::Added(task.text.clone())
        });
    }
    None
}

pub fn compaction_label(item: &Value) -> String {
    if item["status"] == "loading" {
        return "Compacting...".into();
    }
    match item["trigger"].as_str() {
        Some("auto") => "Context automatically compacted".into(),
        Some("manual") => "Context manually compacted".into(),
        _ => match item["preTokens"].as_f64() {
            Some(tokens) if tokens > 0. => {
                format!("Context compacted ({}K tokens)", (tokens / 1000.).round())
            }
            _ => "Context compacted".into(),
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationLevel {
    Info,
    Warning,
    Error,
}

pub fn notification(item: &Value) -> (NotificationLevel, String) {
    let level = match item["level"].as_str() {
        Some("warning") => NotificationLevel::Warning,
        Some("error") => NotificationLevel::Error,
        _ => NotificationLevel::Info,
    };
    let message = item["message"].as_str().unwrap_or("").to_owned();
    (level, message)
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolPresentation {
    pub display_name: String,
    pub summary: Option<String>,
    pub error_text: Option<String>,
    pub icon: &'static str,
    pub loading: bool,
    pub can_open: bool,
}

fn humanize_tool_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return name.to_owned();
    }
    if trimmed.contains([':', '.', '/']) || trimmed.contains("__") {
        return trimmed.to_owned();
    }
    let normalized = trimmed
        .split([' ', '_', '-'])
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = normalized.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    first.to_uppercase().collect::<String>() + chars.as_str()
}

fn error_text(error: &Value) -> Option<String> {
    match error {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        Value::Object(fields) => fields
            .get("content")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| serde_json::to_string_pretty(error).ok()),
        value => Some(value.to_string()),
    }
}

fn strip_cwd_prefix(path: &str, cwd: Option<&str>) -> String {
    let Some(cwd) = cwd.map(str::trim).filter(|cwd| !cwd.is_empty()) else {
        return path.into();
    };
    let prefix = if cwd.ends_with('/') {
        cwd.to_owned()
    } else {
        format!("{cwd}/")
    };
    path.strip_prefix(&prefix).unwrap_or(path).to_owned()
}

pub fn tool_presentation(item: &Value, cwd: Option<&str>) -> ToolPresentation {
    let name = item["name"].as_str().unwrap_or("");
    let status = item["status"].as_str().unwrap_or("completed");
    let detail = &item["detail"];
    let detail_type = detail["type"].as_str().unwrap_or("unknown");
    let file_path = |value: &Value| value.as_str().map(|path| strip_cwd_prefix(path, cwd));
    let (display_name, summary, icon) = match detail_type {
        "shell" => (
            "Shell".to_owned(),
            detail["command"].as_str().map(str::to_owned),
            "square-terminal",
        ),
        "read" => (
            "Read".to_owned(),
            file_path(&detail["filePath"]),
            "file-text",
        ),
        "edit" => ("Edit".to_owned(), file_path(&detail["filePath"]), "pencil"),
        "write" => ("Write".to_owned(), file_path(&detail["filePath"]), "pencil"),
        "search" => (
            "Search".to_owned(),
            detail["query"].as_str().map(str::to_owned),
            "search",
        ),
        "fetch" => (
            "Fetch".to_owned(),
            detail["url"].as_str().map(str::to_owned),
            "search",
        ),
        "worktree_setup" => (
            "Worktree setup".to_owned(),
            detail["branchName"].as_str().map(str::to_owned),
            "square-terminal",
        ),
        "sub_agent" => (
            detail["subAgentType"]
                .as_str()
                .filter(|value| !value.is_empty())
                .unwrap_or("Task")
                .to_owned(),
            detail["description"].as_str().map(str::to_owned),
            "bot",
        ),
        "plain_text" => (
            humanize_tool_name(name),
            detail["label"].as_str().map(str::to_owned),
            "wrench",
        ),
        "plan" => ("Plan".to_owned(), None, "brain"),
        _ => {
            let lower = name.trim().to_lowercase();
            if lower == "thinking" {
                ("Thinking".to_owned(), None, "brain")
            } else if lower == "speak" {
                (humanize_tool_name(name), None, "mic")
            } else if lower == "task" {
                (
                    "Task".to_owned(),
                    item["metadata"]["subAgentActivity"]
                        .as_str()
                        .map(str::to_owned),
                    "bot",
                )
            } else if lower == "terminal" {
                (
                    "Terminal".to_owned(),
                    (detail_type == "plain_text")
                        .then(|| detail["label"].as_str())
                        .flatten()
                        .map(str::to_owned),
                    "square-terminal",
                )
            } else {
                (humanize_tool_name(name), None, "wrench")
            }
        }
    };
    let error_text = if status == "failed" {
        error_text(&item["error"])
    } else {
        None
    };
    let has_detail = detail.as_object().is_some_and(|detail| !detail.is_empty());
    let loading = matches!(status, "running" | "executing");
    ToolPresentation {
        display_name,
        summary,
        error_text,
        icon,
        loading,
        can_open: has_detail || loading,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Footer {
    pub turn: String,
    pub copy_text: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub key: u64,
    pub kind: Kind,
    pub item: Value,
    pub timestamp: Option<String>,
    pub turn: Option<String>,
    pub gap: f32,
    pub activity: Option<Activity>,
    pub footer: Option<Footer>,
}

/// Gap between two consecutive rows, matching `getGapBetweenStreamItems`.
fn row_gap(row: Kind, below: Option<Kind>) -> f32 {
    let Some(below) = below else {
        return 0.;
    };
    match (row, below) {
        (Kind::User, Kind::User) => 4.,
        (Kind::User, Kind::Assistant) => 0.,
        (row, below) if row.is_tool_sequence() && below.is_tool_sequence() => 0.,
        (Kind::User, below) if below.is_tool_sequence() => BLOCK_GAP,
        (Kind::Assistant, below) if below.is_tool_sequence() => 4.,
        (row, Kind::Assistant) if row.is_tool_sequence() => 4.,
        _ => BLOCK_GAP,
    }
}

/// Assistant text for one turn, used when a streaming message updates the turn footer in place.
pub fn copy_for_turn(entries: &BTreeMap<u64, Entry>, turn: Option<&str>, anchor: u64) -> String {
    let assistant = |entry: &Entry| entry.item["type"] == "assistant_message";
    let text = |entry: &Entry| entry.item["text"].as_str().unwrap_or("").to_owned();
    if let Some(turn) = turn {
        return entries
            .values()
            .filter(|entry| entry.turn_id.as_deref() == Some(turn) && assistant(entry))
            .map(text)
            .collect::<Vec<_>>()
            .join("\n\n");
    }
    let mut start = anchor;
    for (key, entry) in entries.range(..=anchor).rev() {
        if *key != anchor && is_turn_trigger(&entry.item) {
            break;
        }
        start = *key;
    }
    entries
        .range(start..=anchor)
        .filter(|(_, entry)| assistant(entry))
        .map(|(_, entry)| text(entry))
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub fn duration_between(from: Option<&str>, to: Option<&str>) -> Option<u64> {
    millis(from, to)
}

pub fn now() -> DateTime<FixedOffset> {
    Local::now().fixed_offset()
}

pub fn parse_time(raw: Option<&str>) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(raw?).ok()
}

/// Display timestamps in the machine's current local offset, matching the app's local time.
pub fn timestamp_label(timestamp: &str, now: DateTime<FixedOffset>) -> String {
    let Some(time) = parse_time(Some(timestamp)) else {
        return String::new();
    };
    let offset = *now.offset();
    let time = time.with_timezone(&offset);
    if time.date_naive() == now.date_naive() {
        return time.format("%H:%M").to_string();
    }
    let days = (now.date_naive() - time.date_naive()).num_days();
    if (1..7).contains(&days) {
        return time.format("%A %H:%M").to_string();
    }
    let date = time.format("%e %b %Y").to_string();
    format!("{}, {}", date.trim(), time.format("%H:%M"))
}

pub fn duration_label(duration_ms: Option<u64>) -> String {
    let Some(duration_ms) = duration_ms else {
        return String::new();
    };
    let seconds = duration_ms / 1000;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        let rest = seconds % 60;
        return if rest == 0 {
            format!("{minutes}m")
        } else {
            format!("{minutes}m {rest}s")
        };
    }
    let hours = minutes / 60;
    let rest = minutes % 60;
    if rest == 0 {
        format!("{hours}h")
    } else {
        format!("{hours}h {rest}m")
    }
}

fn millis(from: Option<&str>, to: Option<&str>) -> Option<u64> {
    let start = parse_time(from)?;
    let end = parse_time(to)?;
    u64::try_from((end - start).num_milliseconds().max(0)).ok()
}

/// Whether the daemon echoed a just-sent message into the timeline. The client send id is
/// preserved as `clientMessageId`; `messageId` is only added when a provider assigns its own.
pub fn has_pending_echo(entries: &BTreeMap<u64, Entry>, message_id: &str) -> bool {
    entries.values().any(|entry| {
        entry.item["type"] == "user_message"
            && (entry.item["clientMessageId"].as_str() == Some(message_id)
                || entry.item["messageId"].as_str() == Some(message_id))
    })
}

pub fn rows(entries: &BTreeMap<u64, Entry>) -> Vec<Row> {
    let ordered = entries.values().collect::<Vec<_>>();
    let mut rows = Vec::with_capacity(ordered.len());
    let mut last_todo: Option<Vec<Task>> = None;
    for (index, entry) in ordered.iter().enumerate() {
        let kind = Kind::from_item(&entry.item);
        let below = ordered
            .get(index + 1)
            .map(|entry| Kind::from_item(&entry.item));
        let activity = if kind == Kind::Todo {
            let current = tasks(&entry.item);
            let activity = activity(last_todo.as_deref(), &current);
            last_todo = Some(current);
            activity
        } else {
            None
        };
        rows.push(Row {
            key: entry.seq_start,
            kind,
            item: entry.item.clone(),
            timestamp: entry.timestamp.clone(),
            turn: entry.turn_id.clone(),
            gap: row_gap(kind, below),
            activity,
            footer: None,
        });
    }
    let mut index = 0;
    while index < ordered.len() {
        let mut end = index + 1;
        while end < ordered.len() && continues(ordered[end - 1], ordered[end]) {
            end += 1;
        }
        let turn_rows = &rows[index..end];
        if turn_rows.iter().any(|row| row.kind == Kind::Assistant) {
            let last = end - 1;
            let copy_text = turn_rows
                .iter()
                .filter(|row| row.kind == Kind::Assistant)
                .filter_map(|row| row.item["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            let turn = turn_rows
                .iter()
                .find_map(|row| row.turn.clone())
                .unwrap_or_else(|| format!("seq-{}", rows[index].key));
            let started_at = turn_rows
                .iter()
                .find(|row| row.kind == Kind::User)
                .and_then(|row| row.timestamp.clone());
            rows[last].footer = Some(Footer {
                turn,
                copy_text,
                duration_ms: millis(started_at.as_deref(), rows[last].timestamp.as_deref()),
                started_at,
                completed_at: rows[last].timestamp.clone(),
            });
        }
        index = end;
    }
    rows
}

fn continues(previous: &Entry, next: &Entry) -> bool {
    match (&previous.turn_id, &next.turn_id) {
        (Some(previous), Some(next)) => previous == next,
        _ => Kind::from_item(&next.item) != Kind::User,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(seq: u64, item: Value, timestamp: &str, turn: &str) -> Entry {
        serde_json::from_value(json!({
            "seqStart": seq,
            "seqEnd": seq,
            "item": item,
            "timestamp": timestamp,
            "turnId": turn,
            "sourceSeqRanges": [{"startSeq": seq, "endSeq": seq}],
        }))
        .unwrap()
    }

    fn history(items: &[(Value, &str, &str)]) -> BTreeMap<u64, Entry> {
        items
            .iter()
            .enumerate()
            .map(|(index, (item, timestamp, turn))| {
                let seq = index as u64 + 1;
                (seq, entry(seq, item.clone(), timestamp, turn))
            })
            .collect()
    }

    fn rows_for(items: &[(Value, &str, &str)]) -> Vec<Row> {
        rows(&history(items))
    }

    #[test]
    fn pending_echo_uses_the_client_message_id_and_falls_back() {
        let mut entries = BTreeMap::new();
        entries.insert(
            1,
            entry(
                1,
                json!({"type":"user_message","text":"Hi","clientMessageId":"client-1"}),
                "2026-01-01T10:00:00Z",
                "t1",
            ),
        );
        assert!(has_pending_echo(&entries, "client-1"));
        assert!(!has_pending_echo(&entries, "provider-1"));
        let mut legacy = BTreeMap::new();
        legacy.insert(
            1,
            entry(
                1,
                json!({"type":"user_message","text":"Hi","messageId":"provider-1"}),
                "2026-01-01T10:00:00Z",
                "t1",
            ),
        );
        assert!(has_pending_echo(&legacy, "provider-1"));
    }

    #[test]
    fn completed_turns_fold_under_a_summary_and_open_on_demand() {
        let items = vec![
            (
                json!({"type":"user_message","text":"First"}),
                "2026-01-01T10:00:00Z",
                "t1",
            ),
            (
                json!({"type":"reasoning","text":"plan"}),
                "2026-01-01T10:00:01Z",
                "t1",
            ),
            (
                json!({"type":"tool_call","name":"shell","status":"completed","detail":{"type":"shell","command":"ls"}}),
                "2026-01-01T10:00:02Z",
                "t1",
            ),
            (
                json!({"type":"assistant_message","text":"First answer"}),
                "2026-01-01T10:00:03Z",
                "t1",
            ),
            (
                json!({"type":"user_message","text":"Second"}),
                "2026-01-01T10:01:00Z",
                "t2",
            ),
            (
                json!({"type":"tool_call","name":"shell","status":"completed","detail":{"type":"shell","command":"pwd"}}),
                "2026-01-01T10:01:01Z",
                "t2",
            ),
            (
                json!({"type":"assistant_message","text":"Second answer"}),
                "2026-01-01T10:01:02Z",
                "t2",
            ),
        ];
        let entries = history(&items);
        let display = display_rows(&entries, &BTreeSet::new(), None);
        assert_eq!(display.len(), 6);
        assert!(
            matches!(display[1], DisplayRow::Summary { summary: ref s, expanded: false, .. } if summary_label(s) == "Ran 1 command")
        );
        assert!(
            matches!(&display[2], DisplayRow::Item { row, nested: false } if row.kind == Kind::Assistant)
        );
        assert!(matches!(&display[3], DisplayRow::Item { row, .. } if row.kind == Kind::User));
        // The second answer is the last row of the conversation and stays visible.
        assert!(matches!(&display[5], DisplayRow::Item { row, .. } if row.kind == Kind::Assistant));

        let host = display[0].item().unwrap().key;
        let expanded = display_rows(&entries, &BTreeSet::from([host]), None);
        assert_eq!(expanded.len(), 8);
        assert!(
            matches!(&expanded[2], DisplayRow::Item { row, nested: true } if row.kind == Kind::Reasoning)
        );
        assert!(
            matches!(&expanded[3], DisplayRow::Item { row, nested: true } if row.kind == Kind::Tool)
        );

        // The active turn stays open, so its work is not folded.
        let active = display_rows(&entries, &BTreeSet::new(), Some("t2"));
        assert_eq!(active.len(), 6);
        assert!(
            matches!(&active[4], DisplayRow::Item { row, nested: false } if row.kind == Kind::Tool)
        );
    }

    #[test]
    fn summary_labels_match_the_desktop_wording() {
        let summary = TurnSummary {
            thought_count: 2,
            edited_file_count: 2,
            command_count: 3,
            read_file_count: 1,
            search_count: 0,
            other_tool_count: 0,
            paseo_actions: BTreeMap::from([("Sent message".to_owned(), 2)]),
        };
        assert_eq!(
            summary_label(&summary),
            "Edited 2 files, ran 3 commands, read 1 file, and sent message ×2"
        );
        assert_eq!(
            summary_label(&TurnSummary {
                thought_count: 1,
                ..Default::default()
            }),
            "Thought"
        );
        assert_eq!(summary_label(&TurnSummary::default()), "View turn details");
    }

    #[test]
    fn turn_footer_uses_the_last_row_time_and_user_trigger() {
        let rows = rows_for(&[
            (
                json!({"type":"user_message","text":"Hi"}),
                "2026-01-01T10:00:00Z",
                "t1",
            ),
            (
                json!({"type":"assistant_message","text":"Hello"}),
                "2026-01-01T10:00:05Z",
                "t1",
            ),
            (
                json!({"type":"tool_call","name":"shell","status":"completed","detail":{"type":"shell","command":"ls"}}),
                "2026-01-01T10:00:07Z",
                "t1",
            ),
            (
                json!({"type":"assistant_message","text":"Done"}),
                "2026-01-01T10:00:12Z",
                "t1",
            ),
            (
                json!({"type":"user_message","text":"Next"}),
                "2026-01-01T10:01:00Z",
                "t2",
            ),
            (
                json!({"type":"assistant_message","text":"Second"}),
                "2026-01-01T10:01:03Z",
                "t2",
            ),
        ]);
        assert_eq!(rows[3].footer.as_ref().unwrap().copy_text, "Hello\n\nDone");
        assert_eq!(rows[3].footer.as_ref().unwrap().duration_ms, Some(12_000));
        assert_eq!(
            rows[3].footer.as_ref().unwrap().completed_at.as_deref(),
            Some("2026-01-01T10:00:12Z")
        );
        assert_eq!(rows[3].gap, BLOCK_GAP);
        assert_eq!(rows[4].gap, 0.);
        assert_eq!(rows[5].footer.as_ref().unwrap().duration_ms, Some(3_000));
        assert_eq!(duration_label(Some(3_000)), "3s");
        assert_eq!(duration_label(Some(83_000)), "1m 23s");
        assert_eq!(duration_label(Some(3_600_000)), "1h");
    }

    #[test]
    fn gaps_match_the_authored_markdown_rhythm() {
        let rows = rows_for(&[
            (
                json!({"type":"user_message","text":"a"}),
                "2026-01-01T10:00:00Z",
                "t1",
            ),
            (
                json!({"type":"assistant_message","text":"b"}),
                "2026-01-01T10:00:01Z",
                "t1",
            ),
            (
                json!({"type":"reasoning","text":"c"}),
                "2026-01-01T10:00:02Z",
                "t1",
            ),
            (
                json!({"type":"tool_call","name":"x","status":"completed","detail":{}}),
                "2026-01-01T10:00:03Z",
                "t1",
            ),
            (
                json!({"type":"todo","items":[]}),
                "2026-01-01T10:00:04Z",
                "t1",
            ),
            (
                json!({"type":"assistant_message","text":"d"}),
                "2026-01-01T10:00:05Z",
                "t1",
            ),
            (
                json!({"type":"user_message","text":"e"}),
                "2026-01-01T10:00:06Z",
                "t2",
            ),
        ]);
        assert_eq!(rows[0].gap, 0.);
        assert_eq!(rows[1].gap, 4.);
        assert_eq!(rows[2].gap, 0.);
        assert_eq!(rows[3].gap, 0.);
        assert_eq!(rows[4].gap, 4.);
        assert_eq!(rows[5].gap, BLOCK_GAP);
        assert_eq!(rows[5].footer.as_ref().unwrap().copy_text, "b\n\nd");
    }

    #[test]
    fn todo_activity_reports_only_the_first_change_of_a_row() {
        let first = json!({"type":"todo","items":[{"text":"one","completed":false,"status":"pending"},{"text":"two","completed":false}]});
        let second = json!({"type":"todo","items":[{"text":"one","completed":false,"status":"in_progress"},{"text":"two","completed":false}]});
        let third = json!({"type":"todo","items":[{"text":"one","completed":true,"status":"completed"},{"text":"two","completed":false}]});
        let rows = rows_for(&[
            (first, "2026-01-01T10:00:00Z", "t1"),
            (second, "2026-01-01T10:00:01Z", "t1"),
            (third.clone(), "2026-01-01T10:00:02Z", "t1"),
        ]);
        assert_eq!(rows[0].activity, Some(Activity::Created(2)));
        assert_eq!(rows[1].activity, Some(Activity::Started("one".into())));
        assert_eq!(rows[2].activity, Some(Activity::Completed("one".into())));
        let tasks = tasks(&third);
        assert_eq!(tasks[0].label(), "one");
        assert!(tasks[0].is_complete());
        assert!(!tasks[1].is_running());
    }

    #[test]
    fn tool_labels_follow_canonical_details_and_unknown_overrides() {
        let shell = json!({"type":"tool_call","name":"Bash","status":"running","detail":{"type":"shell","command":"ls -la"}});
        let presentation = tool_presentation(&shell, None);
        assert_eq!(presentation.display_name, "Shell");
        assert_eq!(presentation.summary.as_deref(), Some("ls -la"));
        assert!(presentation.loading);
        assert!(presentation.can_open);
        let read = json!({"type":"tool_call","name":"Read","status":"completed","detail":{"type":"read","filePath":"/repo/src/main.rs"}});
        assert_eq!(
            tool_presentation(&read, Some("/repo")).summary.as_deref(),
            Some("src/main.rs")
        );
        let thinking = json!({"type":"tool_call","name":"thinking","status":"completed","detail":{"type":"unknown","input":"why","output":null}});
        assert_eq!(tool_presentation(&thinking, None).display_name, "Thinking");
        let failed = json!({"type":"tool_call","name":"future_tool","status":"failed","error":{"content":"boom"},"detail":{"type":"unknown","input":{},"output":null}});
        let presentation = tool_presentation(&failed, None);
        assert_eq!(presentation.display_name, "Future tool");
        assert_eq!(presentation.error_text.as_deref(), Some("boom"));
        assert!(!presentation.loading);
    }

    #[test]
    fn compaction_labels_and_timestamps_match_the_app_wording() {
        assert_eq!(
            compaction_label(&json!({"status":"loading"})),
            "Compacting..."
        );
        assert_eq!(
            compaction_label(&json!({"status":"completed","trigger":"auto"})),
            "Context automatically compacted"
        );
        assert_eq!(
            compaction_label(&json!({"status":"completed","preTokens":12345})),
            "Context compacted (12K tokens)"
        );
        let now = parse_time(Some("2026-11-02T10:00:00+08:00")).unwrap();
        assert_eq!(timestamp_label("2026-11-02T01:30:00Z", now), "09:30");
        assert_eq!(timestamp_label("2026-10-30T01:30:00Z", now), "Friday 09:30");
        assert_eq!(
            timestamp_label("2026-10-01T01:30:00Z", now),
            "1 Oct 2026, 09:30"
        );
        assert_eq!(timestamp_label("not a time", now), "");
    }

    #[test]
    fn missing_turn_ids_fall_back_to_user_message_boundaries() {
        let mut entries = history(&[
            (
                json!({"type":"user_message","text":"one"}),
                "2026-01-01T10:00:00Z",
                "t1",
            ),
            (
                json!({"type":"assistant_message","text":"a"}),
                "2026-01-01T10:00:01Z",
                "t1",
            ),
        ]);
        for entry in entries.values_mut() {
            entry.turn_id = None;
        }
        let rows = rows(&entries);
        assert_eq!(rows[1].footer.as_ref().unwrap().duration_ms, Some(1_000));
        assert_eq!(rows[1].footer.as_ref().unwrap().turn, "seq-1");
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TurnSummary {
    pub thought_count: usize,
    pub edited_file_count: usize,
    pub command_count: usize,
    pub read_file_count: usize,
    pub search_count: usize,
    pub other_tool_count: usize,
    pub paseo_actions: BTreeMap<String, usize>,
}

impl TurnSummary {
    pub fn has_work(&self) -> bool {
        self.edited_file_count > 0
            || self.command_count > 0
            || self.read_file_count > 0
            || self.search_count > 0
            || self.other_tool_count > 0
            || !self.paseo_actions.is_empty()
    }
}

#[derive(Clone, Debug)]
pub enum DisplayRow {
    Item {
        row: Row,
        nested: bool,
    },
    Summary {
        host: u64,
        summary: TurnSummary,
        expanded: bool,
    },
}

impl DisplayRow {
    pub fn item(&self) -> Option<&Row> {
        match self {
            Self::Item { row, .. } => Some(row),
            Self::Summary { .. } => None,
        }
    }
}

fn is_turn_trigger(item: &Value) -> bool {
    item["type"] == "user_message"
        || (item["type"] == "tool_call"
            && (item
                .get("agentMessage")
                .is_some_and(|message| !message.is_null())
                || item["name"] == "heartbeat"))
}

fn stays_visible_when_collapsed(row: &Row) -> bool {
    is_turn_trigger(&row.item) || row.kind == Kind::Compaction
}

fn paseo_action_label(name: &str) -> Option<&'static str> {
    let normalized = name.trim().to_lowercase();
    let leaf = if normalized.contains("__") {
        let segments = normalized
            .split("__")
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        if segments.len() >= 3
            && segments[0] == "mcp"
            && (segments[1] == "paseo" || segments[1].starts_with("paseo_"))
        {
            segments[2..].join("__")
        } else {
            return None;
        }
    } else if normalized.contains('.') {
        let mut segments = normalized.split('.');
        let first = segments.next()?;
        if first == "paseo" || first.starts_with("paseo_") {
            segments.collect::<Vec<_>>().join(".")
        } else {
            return None;
        }
    } else {
        return None;
    };
    PASEO_ACTION_LABELS
        .iter()
        .find(|(tool, _)| *tool == leaf.as_str())
        .map(|(_, label)| *label)
}

fn is_direct_search(normalized: &str) -> bool {
    ["web_search", "llm_context"].iter().any(|suffix| {
        normalized == *suffix
            || normalized.ends_with(&format!("_{suffix}"))
            || normalized.ends_with(&format!(".{suffix}"))
            || normalized.ends_with(&format!("/{suffix}"))
            || normalized.ends_with(&format!(":{suffix}"))
    })
}

fn summarize_turn(rows: &[Row]) -> TurnSummary {
    let mut summary = TurnSummary::default();
    let mut edited_files = BTreeSet::new();
    let mut read_files = BTreeSet::new();
    for row in rows {
        match row.kind {
            Kind::Reasoning => summary.thought_count += 1,
            Kind::Tool => {
                if is_turn_trigger(&row.item) {
                    continue;
                }
                let detail = &row.item["detail"];
                let name = row.item["name"].as_str().unwrap_or("");
                let normalized = name.trim().to_lowercase();
                if let Some(label) = paseo_action_label(name) {
                    *summary.paseo_actions.entry(label.to_owned()).or_insert(0) += 1;
                } else {
                    match detail["type"].as_str().unwrap_or("unknown") {
                        "edit" | "write" => {
                            if let Some(path) = detail["filePath"].as_str() {
                                edited_files.insert(path.to_owned());
                            }
                        }
                        "shell" => summary.command_count += 1,
                        "read" => {
                            if let Some(path) = detail["filePath"].as_str() {
                                read_files.insert(path.to_owned());
                            }
                        }
                        "search" => summary.search_count += 1,
                        _ if is_direct_search(&normalized) => summary.search_count += 1,
                        _ => summary.other_tool_count += 1,
                    }
                }
            }
            _ => {}
        }
    }
    summary.edited_file_count = edited_files.len();
    summary.read_file_count = read_files.len();
    summary
}

fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    first.to_uppercase().collect::<String>() + chars.as_str()
}

/// The one-line description of a folded turn, worded like the desktop summary line.
pub fn summary_label(summary: &TurnSummary) -> String {
    let mut parts = Vec::new();
    let count = |value: usize, one: &str, many: &str| {
        if value == 0 {
            None
        } else if value == 1 {
            Some(one.replace("{count}", "1"))
        } else {
            Some(many.replace("{count}", &value.to_string()))
        }
    };
    parts.extend(count(
        summary.edited_file_count,
        "edited 1 file",
        "edited {count} files",
    ));
    if let Some(part) = count(
        summary.command_count,
        "ran 1 command",
        "ran {count} commands",
    ) {
        parts.push(part);
    }
    parts.extend(count(
        summary.read_file_count,
        "read 1 file",
        "read {count} files",
    ));
    parts.extend(count(
        summary.search_count,
        "searched 1 time",
        "searched {count} times",
    ));
    parts.extend(count(
        summary.other_tool_count,
        "used 1 other tool",
        "used {count} other tools",
    ));
    for (label, count) in &summary.paseo_actions {
        parts.push(if *count == 1 {
            label.to_lowercase()
        } else {
            format!("{} ×{count}", label.to_lowercase())
        });
    }
    if parts.is_empty() {
        return if summary.thought_count > 0 {
            "Thought".into()
        } else {
            "View turn details".into()
        };
    }
    let joined = match parts.len() {
        1 => parts[0].clone(),
        2 => format!("{} and {}", parts[0], parts[1]),
        _ => format!(
            "{}, and {}",
            parts[..parts.len() - 1].join(", "),
            parts[parts.len() - 1]
        ),
    };
    capitalize(&joined)
}

/// Display rows with completed turns folded into a summary line. Collapsing follows the
/// canonical turn ids: a turn folds only when it is closed, its host is loaded, it has work
/// before its last assistant message, and that message is not the turn's first row.
pub fn display_rows(
    entries: &BTreeMap<u64, Entry>,
    expanded_hosts: &BTreeSet<u64>,
    active_turn: Option<&str>,
) -> Vec<DisplayRow> {
    let rows = rows(entries);
    let mut result = Vec::new();
    let mut start = 0;
    while start < rows.len() {
        let mut end = start + 1;
        while end < rows.len() && continues(&entries[&rows[end - 1].key], &entries[&rows[end].key])
        {
            end += 1;
        }
        let group = &rows[start..end];
        let latest = end == rows.len();
        let latest_turn = group[0].turn.as_deref();
        let latest_open = latest
            && active_turn.is_some()
            && (latest_turn.is_none() || active_turn == latest_turn);
        let reply = group.iter().rposition(|row| row.kind == Kind::Assistant);
        let folds =
            !latest_open && is_turn_trigger(&group[0].item) && reply.is_some_and(|reply| reply > 1);
        if !folds {
            result.extend(
                group
                    .iter()
                    .cloned()
                    .map(|row| DisplayRow::Item { row, nested: false }),
            );
            start = end;
            continue;
        }
        let reply = reply.unwrap();
        let folded = &group[1..reply];
        let summary = summarize_turn(folded);
        if !summary.has_work() && !folded.iter().any(|row| row.kind == Kind::Assistant) {
            result.extend(
                group
                    .iter()
                    .cloned()
                    .map(|row| DisplayRow::Item { row, nested: false }),
            );
            start = end;
            continue;
        }
        let host = &group[0];
        let expanded = expanded_hosts.contains(&host.key);
        result.push(DisplayRow::Item {
            row: host.clone(),
            nested: false,
        });
        result.push(DisplayRow::Summary {
            host: host.key,
            summary,
            expanded,
        });
        for row in folded {
            if expanded || stays_visible_when_collapsed(row) {
                result.push(DisplayRow::Item {
                    row: row.clone(),
                    nested: expanded,
                });
            }
        }
        result.extend(
            group[reply..]
                .iter()
                .cloned()
                .map(|row| DisplayRow::Item { row, nested: false }),
        );
        start = end;
    }
    let kinds = result
        .iter()
        .map(|entry| entry.item().map(|row| row.kind))
        .collect::<Vec<_>>();
    for index in 0..result.len() {
        let Some(kind) = result[index].item().map(|row| row.kind) else {
            continue;
        };
        let next = kinds[index + 1..].iter().flatten().copied().next();
        let gap = row_gap(kind, next);
        if let DisplayRow::Item { row, .. } = &mut result[index] {
            row.gap = gap;
        }
    }
    result
}

const PASEO_ACTION_LABELS: &[(&str, &str)] = &[
    ("create_agent", "Created agent"),
    ("send_agent_prompt", "Sent message"),
    ("get_agent_status", "Checked agent"),
    ("get_agent_activity", "Read agent activity"),
    ("list_agents", "Listed agents"),
    ("update_agent", "Updated agent"),
    ("set_agent_mode", "Set agent mode"),
    ("cancel_agent", "Canceled agent"),
    ("archive_agent", "Archived agent"),
    ("kill_agent", "Killed agent"),
    ("list_pending_permissions", "Listed permissions"),
    ("respond_to_permission", "Responded to permission"),
    ("create_workspace", "Created workspace"),
    ("list_hosts", "Listed hosts"),
    ("list_workspaces", "Listed workspaces"),
    ("rename_workspace", "Renamed workspace"),
    ("archive_workspace", "Archived workspace"),
    ("list_workspace_scripts", "Listed scripts"),
    ("start_workspace_script", "Started script"),
    ("stop_workspace_script", "Stopped script"),
    ("create_terminal", "Created terminal"),
    ("list_terminals", "Listed terminals"),
    ("capture_terminal", "Read terminal"),
    ("send_terminal_keys", "Sent keys"),
    ("kill_terminal", "Killed terminal"),
    ("create_schedule", "Created schedule"),
    ("create_heartbeat", "Created heartbeat"),
    ("update_schedule", "Updated schedule"),
    ("list_schedules", "Listed schedules"),
    ("inspect_schedule", "Inspected schedule"),
    ("schedule_logs", "Read schedule logs"),
    ("run_schedule_once", "Ran schedule"),
    ("pause_schedule", "Paused schedule"),
    ("resume_schedule", "Resumed schedule"),
    ("delete_schedule", "Deleted schedule"),
    ("delete_heartbeat", "Deleted heartbeat"),
    ("list_providers", "Listed providers"),
    ("list_models", "Listed models"),
    ("list_profiles", "Listed profiles"),
    ("inspect_provider", "Inspected provider"),
    ("browser_list_tabs", "Listed browser tabs"),
    ("browser_new_tab", "Opened browser tab"),
    ("browser_close_tab", "Closed browser tab"),
    ("browser_navigate", "Navigated"),
    ("browser_back", "Went back"),
    ("browser_forward", "Went forward"),
    ("browser_reload", "Reloaded page"),
    ("browser_snapshot", "Snapshotted page"),
    ("browser_screenshot", "Took screenshot"),
    ("browser_click", "Clicked element"),
    ("browser_hover", "Hovered element"),
    ("browser_drag", "Dragged element"),
    ("browser_fill", "Filled field"),
    ("browser_select", "Selected option"),
    ("browser_type", "Typed text"),
    ("browser_keypress", "Pressed key"),
    ("browser_upload", "Uploaded files"),
    ("browser_wait", "Waited for page"),
    ("browser_scroll", "Scrolled page"),
    ("browser_resize", "Resized browser"),
    ("browser_logs", "Read browser logs"),
    ("browser_evaluate", "Ran JavaScript"),
];
