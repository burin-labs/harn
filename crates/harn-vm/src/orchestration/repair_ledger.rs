use serde::Serialize;
use serde_json::Value;

const SCHEMA: &str = "harn.agent.repair_ledger.v1";
const MAX_FIXES: usize = 8;
const MAX_DIAGNOSTICS: usize = 12;
const MAX_TEXT_CHARS: usize = 220;

#[derive(Clone, Debug, Serialize)]
struct RepairFix {
    file: String,
    summary: String,
    status: &'static str,
    message_index: usize,
}

#[derive(Clone, Debug, Serialize)]
struct RepairLoop {
    file: String,
    kind: &'static str,
    count: usize,
}

#[derive(Debug, Serialize)]
struct RepairLedger {
    schema: &'static str,
    fixes: Vec<RepairFix>,
    current_diagnostics: Vec<String>,
    open_loops: Vec<RepairLoop>,
}

pub(super) fn append_repair_ledger_to_summary(summary: String, archived: &[Value]) -> String {
    let Some(ledger) = build_repair_ledger(archived) else {
        return summary;
    };
    let Ok(encoded) = serde_json::to_string_pretty(&ledger) else {
        return summary;
    };
    format!("{summary}\n\n[repair-ledger {SCHEMA}]\n```json\n{encoded}\n```")
}

fn build_repair_ledger(archived: &[Value]) -> Option<RepairLedger> {
    let mut fixes = Vec::new();
    let mut current_diagnostics = Vec::new();
    let mut verify_success_indices = Vec::new();

    for (message_index, message) in archived.iter().enumerate() {
        let text = message_text(message);
        if text.trim().is_empty() {
            continue;
        }
        let tool_name = tool_name(message);
        let diagnostic_lines = failure_lines(&text);
        let has_diagnostics = !diagnostic_lines.is_empty();
        if is_verify_message(tool_name.as_deref(), &text) {
            if !has_diagnostics {
                if has_success_signal(&text) {
                    current_diagnostics.clear();
                    verify_success_indices.push(message_index);
                }
            } else {
                current_diagnostics = diagnostic_lines;
            }
        }
        if is_edit_message(tool_name.as_deref(), &text) && !has_diagnostics {
            fixes.push(RepairFix {
                file: extract_path(&text).unwrap_or_else(|| "unknown".to_string()),
                summary: first_meaningful_line(&text),
                status: "unverified",
                message_index,
            });
        }
    }

    for fix in &mut fixes {
        if verify_success_indices
            .iter()
            .any(|success_index| *success_index > fix.message_index)
        {
            fix.status = "verified-good";
        }
    }

    let open_loops = open_loops_for(&fixes);
    let fixes = bounded_fixes(fixes);
    current_diagnostics.truncate(MAX_DIAGNOSTICS);
    if fixes.is_empty() && current_diagnostics.is_empty() && open_loops.is_empty() {
        return None;
    }

    Some(RepairLedger {
        schema: SCHEMA,
        fixes,
        current_diagnostics,
        open_loops,
    })
}

fn bounded_fixes(mut fixes: Vec<RepairFix>) -> Vec<RepairFix> {
    while fixes.len() > MAX_FIXES {
        if let Some(pos) = fixes.iter().position(|fix| fix.status == "verified-good") {
            fixes.remove(pos);
        } else {
            fixes.remove(0);
        }
    }
    fixes
}

fn open_loops_for(fixes: &[RepairFix]) -> Vec<RepairLoop> {
    let mut loops = Vec::new();
    for fix in fixes.iter().filter(|fix| fix.status != "verified-good") {
        if fix.file == "unknown" {
            continue;
        }
        if loops.iter().any(|item: &RepairLoop| item.file == fix.file) {
            continue;
        }
        let count = fixes
            .iter()
            .filter(|other| other.status != "verified-good" && other.file == fix.file)
            .count();
        if count >= 3 {
            loops.push(RepairLoop {
                file: fix.file.clone(),
                kind: "repeated-unverified-fix",
                count,
            });
        }
    }
    loops
}

fn message_text(message: &Value) -> String {
    let Some(content) = message.get("content") else {
        return value_text(message);
    };
    value_text(content)
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(value_text).collect::<Vec<_>>().join("\n"),
        Value::Object(map) => ["text", "content", "body", "result", "message", "error"]
            .iter()
            .filter_map(|key| map.get(*key))
            .map(value_text)
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        Value::Bool(_) | Value::Number(_) => value.to_string(),
    }
}

fn tool_name(message: &Value) -> Option<String> {
    ["name", "tool_name", "tool"]
        .iter()
        .find_map(|key| message.get(*key).and_then(Value::as_str))
        .map(|name| name.to_ascii_lowercase())
}

fn is_edit_message(tool_name: Option<&str>, text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    tool_name.is_some_and(|name| {
        [
            "edit", "write", "patch", "replace", "create", "delete", "move", "rename",
        ]
        .iter()
        .any(|needle| name.contains(needle))
    }) || [
        "applied edit",
        "updated ",
        "created ",
        "deleted ",
        "wrote ",
        "patched ",
        "replaced ",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn is_verify_message(tool_name: Option<&str>, text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    tool_name.is_some_and(|name| {
        [
            "verify", "test", "check", "build", "run", "exec", "shell", "command",
        ]
        .iter()
        .any(|needle| name.contains(needle))
    }) || [
        "exit code",
        "tests passed",
        "test result",
        "failed",
        "error:",
        "warning:",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn has_success_signal(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "exit code 0",
        "exit status 0",
        "tests passed",
        "test result: ok",
        "all tests passed",
        "finished successfully",
        "build succeeded",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn failure_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| super::compaction::is_failure_signal_line(line))
        .map(trim_for_ledger)
        .take(MAX_DIAGNOSTICS)
        .collect()
}

fn first_meaningful_line(text: &str) -> String {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .map(trim_for_ledger)
        .unwrap_or_else(|| trim_for_ledger(text))
}

fn trim_for_ledger(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= MAX_TEXT_CHARS {
        return trimmed.to_string();
    }
    let mut clipped = trimmed.chars().take(MAX_TEXT_CHARS).collect::<String>();
    clipped.push_str("...");
    clipped
}

fn extract_path(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|token| {
            token.trim_matches(|c: char| {
                matches!(
                    c,
                    '`' | '\'' | '"' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}'
                )
            })
        })
        .find_map(normalize_path_token)
}

fn normalize_path_token(token: &str) -> Option<String> {
    let trimmed = token
        .trim_end_matches('.')
        .trim_end_matches(':')
        .trim_end_matches(',');
    if !(trimmed.contains('/') || trimmed.contains('\\')) {
        return None;
    }
    let path = trim_diagnostic_suffix(trimmed);
    if path.contains('.') || path.contains('/') || path.contains('\\') {
        Some(path.to_string())
    } else {
        None
    }
}

fn trim_diagnostic_suffix(token: &str) -> &str {
    let mut end = token.len();
    for (idx, _) in token.match_indices(':') {
        let rest = &token[idx + 1..];
        if rest.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            end = idx;
            break;
        }
    }
    &token[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn appends_verified_repair_ledger() {
        let archived = vec![
            json!({"role": "tool", "name": "edit", "content": "Applied edit to src/main.cpp: fixed include path"}),
            json!({"role": "tool", "name": "run", "content": "All tests passed\nexit code 0"}),
        ];

        let summary = append_repair_ledger_to_summary("base summary".to_string(), &archived);

        assert!(summary.contains(SCHEMA));
        assert!(summary.contains("src/main.cpp"));
        assert!(summary.contains("verified-good"));
        assert!(summary.contains("\"current_diagnostics\": []"));
    }

    #[test]
    fn keeps_only_current_diagnostics_after_supersession() {
        let archived = vec![
            json!({"role": "tool", "name": "run", "content": "src/old.zig:1:1: error: stale failure"}),
            json!({"role": "tool", "name": "run", "content": "All tests passed\nexit code 0"}),
            json!({"role": "tool", "name": "run", "content": "src/new.zig:7:3: error: current failure"}),
        ];

        let ledger = build_repair_ledger(&archived).expect("ledger");
        let encoded = serde_json::to_string(&ledger).expect("json");

        assert!(encoded.contains("current failure"));
        assert!(!encoded.contains("stale failure"));
    }

    #[test]
    fn leaves_summary_unchanged_without_repair_evidence() {
        let archived = vec![json!({"role": "assistant", "content": "Thinking about the plan."})];

        assert_eq!(
            append_repair_ledger_to_summary("plain".to_string(), &archived),
            "plain"
        );
    }

    #[test]
    fn bounds_fixes_and_preserves_unverified_entries() {
        let archived = (0..12)
            .map(|idx| {
                json!({
                    "role": "tool",
                    "name": "edit",
                    "content": format!("Applied edit to src/file{idx}.rs")
                })
            })
            .collect::<Vec<_>>();

        let ledger = build_repair_ledger(&archived).expect("ledger");

        assert_eq!(ledger.fixes.len(), MAX_FIXES);
        assert!(ledger.fixes[0].file.contains("file4.rs"));
    }
}
