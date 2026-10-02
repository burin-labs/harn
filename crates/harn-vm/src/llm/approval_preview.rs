//! Author-declared approval previews for tools whose arguments do not say what
//! they will do.
//!
//! A tool such as `verify` takes no arguments and resolves its command inside
//! the handler, so the raw input a person sees in a permission request is `{}`.
//! `tool_define` accepts an optional `approval_preview` closure that maps the
//! call's arguments to `{command, cwd?, summary?}`. This module owns that
//! contract: config validation, the fail-safe evaluation, and the typed
//! evidence ref the ACP projection reads.
//!
//! The preview is presentation only. A closure that throws, returns `nil`, or
//! returns a malformed record yields no preview; it never changes whether the
//! call is allowed, and it never rewrites the call's arguments.

use std::cell::Cell;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::value::{VmClosure, VmError, VmValue};

/// The `tool_define` config key and registry-entry field holding the closure.
pub(crate) const CONFIG_KEY: &str = "approval_preview";
/// Evidence-ref `kind` carried on the approval request.
pub(crate) const EVIDENCE_KIND: &str = "command_preview";
/// Largest command or summary a preview may carry. A longer value is treated
/// as malformed rather than truncated: a truncated command misstates what the
/// person approves.
const MAX_FIELD_CHARS: usize = 16 * 1024;

thread_local! {
    /// Re-entrancy guard. A preview closure that itself reaches a host
    /// permission ask must not recurse into another preview.
    static PREVIEW_DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// What a tool will actually do, as declared by the tool's author.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToolApprovalPreview {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl ToolApprovalPreview {
    /// The typed evidence ref appended to the approval request.
    pub(crate) fn evidence_ref(&self) -> JsonValue {
        let mut evidence = serde_json::json!({
            "kind": EVIDENCE_KIND,
            "command": self.command,
            "source": "approval_preview",
        });
        let object = evidence.as_object_mut().expect("evidence object");
        if let Some(cwd) = &self.cwd {
            object.insert("cwd".to_string(), JsonValue::String(cwd.clone()));
        }
        if let Some(summary) = &self.summary {
            object.insert("summary".to_string(), JsonValue::String(summary.clone()));
        }
        evidence
    }

    /// Read a preview back from a `command_preview` evidence ref.
    pub(crate) fn from_evidence_ref(evidence: &JsonValue) -> Option<Self> {
        if evidence.get("kind").and_then(JsonValue::as_str) != Some(EVIDENCE_KIND) {
            return None;
        }
        let field = |key: &str| {
            evidence
                .get(key)
                .and_then(JsonValue::as_str)
                .map(str::to_string)
        };
        Some(Self {
            command: field("command")?,
            cwd: field("cwd"),
            summary: field("summary"),
        })
    }

    /// Plain-text rendering for hosts that only show ACP content blocks.
    pub(crate) fn display_text(&self) -> String {
        let mut lines = vec![format!("Command: {}", self.command)];
        if let Some(cwd) = &self.cwd {
            lines.push(format!("Working directory: {cwd}"));
        }
        if let Some(summary) = &self.summary {
            lines.push(summary.clone());
        }
        lines.join("\n")
    }

    fn from_vm(value: &VmValue) -> Option<Self> {
        if !matches!(value, VmValue::Dict(_)) {
            return None;
        }
        let json = crate::llm::vm_value_to_json(value);
        let mut preview: Self = serde_json::from_value(strip_nulls(json)).ok()?;
        preview.command = preview.command.trim().to_string();
        let valid_len = |text: &str| text.chars().count() <= MAX_FIELD_CHARS;
        if preview.command.is_empty()
            || !valid_len(&preview.command)
            || preview.summary.as_deref().is_some_and(|s| !valid_len(s))
            || preview.cwd.as_deref().is_some_and(|s| !valid_len(s))
        {
            return None;
        }
        preview.cwd = preview.cwd.filter(|cwd| !cwd.is_empty());
        preview.summary = preview.summary.filter(|summary| !summary.is_empty());
        Some(preview)
    }
}

/// `{cwd: nil}` means "absent", matching how Harn record literals spell an
/// omitted optional field.
fn strip_nulls(value: JsonValue) -> JsonValue {
    match value {
        JsonValue::Object(map) => JsonValue::Object(
            map.into_iter()
                .filter(|(_, value)| !value.is_null())
                .collect(),
        ),
        other => other,
    }
}

/// Validate the `approval_preview` config value once, at `tool_define`.
pub(crate) fn validate_config(value: Option<&VmValue>, tool_name: &str) -> Result<(), VmError> {
    match value {
        None | Some(VmValue::Nil) | Some(VmValue::Closure(_)) => Ok(()),
        Some(other) => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!(
                "tool_define: tool {tool_name:?} `{CONFIG_KEY}` must be a closure \
                 `{{ args -> {{command: string, cwd?: string, summary?: string}} }}` \
                 returning nil when there is nothing to preview, got {}",
                other.type_name()
            ),
        )))),
    }
}

/// The preview closure declared on a registry entry, if any.
pub(crate) fn closure_from_entry(entry: &crate::value::DictMap) -> Option<Arc<VmClosure>> {
    match entry.get(CONFIG_KEY) {
        Some(VmValue::Closure(closure)) => Some(closure.clone()),
        _ => None,
    }
}

struct DepthGuard;

impl Drop for DepthGuard {
    fn drop(&mut self) {
        PREVIEW_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

/// Evaluate a tool's preview closure against the call's arguments.
///
/// Fail-safe: no VM context, re-entrancy, a thrown error, `nil`, or a
/// malformed record all yield `None`. Errors are logged, never propagated.
///
/// Callers box this future: it holds a child VM across the closure call, and
/// the dispatch future that awaits a permission request must not grow by it.
pub(crate) async fn evaluate(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    closure: &Arc<VmClosure>,
    tool_name: &str,
    tool_args: &JsonValue,
) -> Option<ToolApprovalPreview> {
    if PREVIEW_DEPTH.with(Cell::get) > 0 {
        return None;
    }
    let mut vm = ctx?.child_vm();
    let arg = crate::json_to_vm_value(tool_args);
    PREVIEW_DEPTH.with(|depth| depth.set(depth.get() + 1));
    let _guard = DepthGuard;
    let result = vm.call_closure_pub(closure, &[arg]).await;
    match result {
        Ok(VmValue::Nil) => None,
        Ok(value) => {
            let preview = ToolApprovalPreview::from_vm(&value);
            if preview.is_none() {
                crate::events::log_warn(
                    "tool.approval_preview",
                    &format!(
                        "tool {tool_name:?} approval_preview returned a value that is not \
                         {{command: string, cwd?: string, summary?: string}}; no preview shown"
                    ),
                );
            }
            preview
        }
        Err(error) => {
            crate::events::log_warn(
                "tool.approval_preview",
                &format!("tool {tool_name:?} approval_preview failed: {error}; no preview shown"),
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(json: JsonValue) -> VmValue {
        crate::json_to_vm_value(&json)
    }

    #[test]
    fn record_round_trips_through_evidence() {
        let preview = ToolApprovalPreview::from_vm(&dict(serde_json::json!({
            "command": " python3 -m pytest ",
            "cwd": "/repo",
            "summary": "Runs the test suite",
        })))
        .expect("valid preview");
        assert_eq!(preview.command, "python3 -m pytest");
        let evidence = preview.evidence_ref();
        assert_eq!(evidence["kind"], "command_preview");
        assert_eq!(
            ToolApprovalPreview::from_evidence_ref(&evidence),
            Some(preview)
        );
    }

    #[test]
    fn malformed_records_yield_no_preview() {
        for value in [
            serde_json::json!({}),
            serde_json::json!({"command": ""}),
            serde_json::json!({"command": 3}),
            serde_json::json!({"command": "ls", "extra": true}),
            serde_json::json!("python3 -m pytest"),
            serde_json::json!({"command": "x".repeat(MAX_FIELD_CHARS + 1)}),
        ] {
            assert_eq!(
                ToolApprovalPreview::from_vm(&dict(value.clone())),
                None,
                "{value}"
            );
        }
        let nil_optional = ToolApprovalPreview::from_vm(&dict(serde_json::json!({
            "command": "ls", "cwd": null
        })))
        .expect("nil optional is absent");
        assert_eq!(nil_optional.cwd, None);
    }

    #[test]
    fn config_must_be_a_closure_or_nil() {
        assert!(validate_config(None, "verify").is_ok());
        assert!(validate_config(Some(&VmValue::Nil), "verify").is_ok());
        let error = validate_config(Some(&VmValue::string("python3")), "verify")
            .expect_err("string preview rejected");
        assert!(format!("{error:?}").contains("approval_preview"));
    }
}
