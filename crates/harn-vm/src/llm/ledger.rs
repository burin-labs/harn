//! Task ledger: durable task-wide state separate from the chat transcript.
//!
//! The ledger is an explicit, structured answer to "what does the user
//! want, what has been delivered, what remains, and why will we say we're
//! done?" It is what the agent consults before emitting `<done>` and what
//! the post-loop QC officer audits the work against.
//!
//! Unlike the chat transcript (ephemeral reasoning) the ledger is durable:
//! it is seeded from the planner, mutated via a `ledger(...)` tool call,
//! rendered into every subsequent prompt, and persisted in run records.
//!
//! This file intentionally stays in one place so the data shape, the
//! tool-call surface, the prompt rendering, and the `<done>` gate all
//! stay in lockstep. If you add a field, add its rendering + gate
//! semantics in the same commit.
//!
//! ## Shape
//!
//! ```text
//! TaskLedger {
//!   root_task: String,
//!   deliverables: Vec<Deliverable>,
//!   rationale: String,
//!   observations: Vec<String>,
//!   grounded_refs: BTreeSet<String>,
//! }
//!
//! Deliverable {
//!   id: String,                 // "deliverable-N" or caller-supplied
//!   text: String,               // what the item is
//!   status: Open|Done|Blocked|Dropped,
//!   note: Option<String>,       // reason on blocked/dropped; prose on done
//! }
//! ```
//!
//! ## `<done>` gate
//!
//! `<done>` is accepted iff no deliverable is in `Open` or `Blocked` state.
//! `Dropped` is the agent's declared escape hatch for scope changes and
//! satisfies the gate while leaving an audit record.

use std::collections::BTreeSet;
use std::rc::Rc;

use serde::{Deserialize, Serialize};

use crate::value::VmValue;

/// Status of a single deliverable. The field ordering matters: it matches
/// the priority ordering used when rendering ledger state into prompts
/// (`Open` comes first so the agent sees pending work at the top).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DeliverableStatus {
    #[default]
    Open,
    Done,
    Blocked,
    Dropped,
}

impl DeliverableStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            DeliverableStatus::Open => "open",
            DeliverableStatus::Done => "done",
            DeliverableStatus::Blocked => "blocked",
            DeliverableStatus::Dropped => "dropped",
        }
    }

    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "open" => Some(Self::Open),
            "done" => Some(Self::Done),
            "blocked" => Some(Self::Blocked),
            "dropped" => Some(Self::Dropped),
            _ => None,
        }
    }

    /// Whether this status keeps the `<done>` gate closed.
    fn blocks_done(self) -> bool {
        matches!(self, DeliverableStatus::Open | DeliverableStatus::Blocked)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct Deliverable {
    pub id: String,
    pub text: String,
    pub status: DeliverableStatus,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct TaskLedger {
    pub root_task: String,
    pub deliverables: Vec<Deliverable>,
    pub rationale: String,
    pub observations: Vec<String>,
    pub grounded_refs: BTreeSet<String>,
}

impl TaskLedger {
    /// True when the ledger has no deliverables or rationale yet.
    /// An empty ledger does not gate `<done>` — the agent is running
    /// without a plan, which is legitimate for trivial one-shots.
    pub(crate) fn is_empty(&self) -> bool {
        self.deliverables.is_empty()
            && self.rationale.trim().is_empty()
            && self.observations.is_empty()
    }

    /// Number of deliverables still preventing `<done>` acceptance.
    pub(crate) fn blocking_count(&self) -> usize {
        self.deliverables
            .iter()
            .filter(|d| d.status.blocks_done())
            .count()
    }

    /// Should `<done>` be rejected right now?
    /// Only gates when the ledger has been meaningfully seeded — an
    /// empty ledger (no deliverables, no rationale) is treated as
    /// "no plan declared, no gate".
    pub(crate) fn gates_done(&self) -> bool {
        !self.deliverables.is_empty() && self.blocking_count() > 0
    }

    /// Render a compact representation for injection into the prompt.
    /// Keeps rendering budget tight: deliverables are one-liners and
    /// the whole block is bounded below ~1500 chars for typical tasks.
    pub(crate) fn render_for_prompt(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut out = String::from("<task_ledger>\n");
        if !self.root_task.trim().is_empty() {
            out.push_str("root_task: ");
            out.push_str(self.root_task.trim());
            out.push('\n');
        }
        if !self.deliverables.is_empty() {
            out.push_str("deliverables:\n");
            for deliverable in &self.deliverables {
                out.push_str("  [");
                out.push_str(&deliverable.id);
                out.push_str("] (");
                out.push_str(deliverable.status.as_str());
                out.push_str(") ");
                out.push_str(deliverable.text.trim());
                if let Some(note) = deliverable.note.as_ref() {
                    let trimmed = note.trim();
                    if !trimmed.is_empty() {
                        out.push_str(" — ");
                        out.push_str(trimmed);
                    }
                }
                out.push('\n');
            }
        }
        if !self.rationale.trim().is_empty() {
            out.push_str("rationale: ");
            out.push_str(self.rationale.trim());
            out.push('\n');
        }
        if !self.observations.is_empty() {
            out.push_str("observations:\n");
            for observation in self
                .observations
                .iter()
                .rev()
                .take(5)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                out.push_str("  - ");
                out.push_str(observation.trim());
                out.push('\n');
            }
        }
        out.push_str("</task_ledger>");
        out
    }

    /// Allocate a fresh deliverable id. Uses the next integer not in use.
    fn next_id(&self) -> String {
        let mut counter = self.deliverables.len() + 1;
        loop {
            let candidate = format!("deliverable-{counter}");
            if !self.deliverables.iter().any(|d| d.id == candidate) {
                return candidate;
            }
            counter += 1;
        }
    }

    /// Apply a `ledger` tool call to mutate state.
    ///
    /// The tool call wire format is:
    /// ```text
    /// ledger({
    ///   action: "add" | "mark" | "rationale" | "note" | "seed_plan",
    ///   id?: string,
    ///   text?: string,
    ///   status?: "done" | "blocked" | "dropped",
    ///   note?: string,
    ///   deliverables?: list<string>,   // for "seed_plan"
    /// })
    /// ```
    pub(crate) fn apply(&mut self, args: &serde_json::Value) -> Result<String, String> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "ledger: `action` is required".to_string())?;
        match action {
            "add" | "add_deliverable" => {
                let text = args
                    .get("text")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "ledger add: `text` is required".to_string())?
                    .trim()
                    .to_string();
                if text.is_empty() {
                    return Err("ledger add: `text` must not be empty".to_string());
                }
                let id = args
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| self.next_id());
                if self.deliverables.iter().any(|d| d.id == id) {
                    return Err(format!(
                        "ledger add: deliverable id {id:?} already exists; use `mark` to update or pick a different id"
                    ));
                }
                self.deliverables.push(Deliverable {
                    id: id.clone(),
                    text,
                    status: DeliverableStatus::Open,
                    note: None,
                });
                Ok(format!("deliverable {id} added"))
            }
            "mark" | "mark_deliverable" => {
                let id = args
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "ledger mark: `id` is required".to_string())?
                    .trim()
                    .to_string();
                let status_raw = args
                    .get("status")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "ledger mark: `status` is required".to_string())?;
                let status = DeliverableStatus::parse(status_raw).ok_or_else(|| {
                    format!("ledger mark: status {status_raw:?} is not one of open|done|blocked|dropped")
                })?;
                let note = args
                    .get("note")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                let known_ids: Vec<String> =
                    self.deliverables.iter().map(|d| d.id.clone()).collect();
                let deliverable = self
                    .deliverables
                    .iter_mut()
                    .find(|d| d.id == id)
                    .ok_or_else(|| {
                        format!(
                            "ledger mark: no deliverable with id {id:?}; known ids: [{}]",
                            known_ids.join(", ")
                        )
                    })?;
                if matches!(
                    status,
                    DeliverableStatus::Blocked | DeliverableStatus::Dropped
                ) && note.is_none()
                {
                    return Err(format!(
                        "ledger mark: status {:?} requires a `note` explaining why",
                        status.as_str()
                    ));
                }
                deliverable.status = status;
                deliverable.note = note;
                Ok(format!("deliverable {id} marked {}", status.as_str()))
            }
            "rationale" => {
                let text = args
                    .get("text")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "ledger rationale: `text` is required".to_string())?
                    .trim()
                    .to_string();
                self.rationale = text;
                Ok("rationale updated".to_string())
            }
            "note" => {
                let text = args
                    .get("text")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "ledger note: `text` is required".to_string())?
                    .trim()
                    .to_string();
                if text.is_empty() {
                    return Err("ledger note: `text` must not be empty".to_string());
                }
                self.observations.push(text);
                Ok("observation recorded".to_string())
            }
            "seed_plan" => {
                let items = args.get("deliverables").and_then(|v| v.as_array());
                let Some(items) = items else {
                    return Err("ledger seed_plan: `deliverables` list is required".to_string());
                };
                if !self.deliverables.is_empty() {
                    return Err(
                        "ledger seed_plan: ledger already has deliverables; use `add` to extend"
                            .to_string(),
                    );
                }
                for (idx, item) in items.iter().enumerate() {
                    let text = item
                        .as_str()
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default();
                    if text.is_empty() {
                        return Err(format!(
                            "ledger seed_plan: deliverables[{idx}] must be a non-empty string"
                        ));
                    }
                    self.deliverables.push(Deliverable {
                        id: format!("deliverable-{}", idx + 1),
                        text,
                        status: DeliverableStatus::Open,
                        note: None,
                    });
                }
                Ok(format!(
                    "ledger seeded with {} deliverables",
                    self.deliverables.len()
                ))
            }
            other => Err(format!(
                "ledger: unknown action {other:?}; use one of add|mark|rationale|note|seed_plan"
            )),
        }
    }

    /// Build the corrective message the runtime emits when the agent
    /// tries to `<done>` while work remains. Names specific open items
    /// so the student model learns the correct next move.
    pub(crate) fn done_gate_feedback(&self) -> String {
        let open: Vec<&Deliverable> = self
            .deliverables
            .iter()
            .filter(|d| d.status.blocks_done())
            .collect();
        if open.is_empty() {
            return String::new();
        }
        let mut out = format!(
            "`<done>` rejected: {} deliverable(s) are not yet resolved.\n",
            open.len()
        );
        for deliverable in open {
            out.push_str(&format!(
                "  [{}] ({}) {}\n",
                deliverable.id,
                deliverable.status.as_str(),
                deliverable.text.trim()
            ));
        }
        out.push_str(
            "Finish these items with real tool calls, mark them with `ledger({ action: \"mark\", id: \"...\", status: \"dropped\", note: \"why\" })` if scope changed, or add a new deliverable with `ledger({ action: \"add\", text: \"...\" })` if the plan evolved. Do NOT emit `<done>` again until the ledger has zero open/blocked items.",
        );
        out
    }
}

/// Record grounded references from the result of an exploration tool.
/// Populates the ledger's `grounded_refs` set so the grounding lint can
/// distinguish "the agent read this" from "the agent guessed".
pub(crate) fn record_grounded_refs(
    ledger: &mut TaskLedger,
    tool_name: &str,
    args: &serde_json::Value,
) {
    // Record arguments that shape what the agent claims to have seen.
    // This is deliberately coarse — we want to over-record file paths
    // and symbol names so the lint rarely false-positives.
    let mut harvest = |value: &serde_json::Value| {
        if let Some(s) = value.as_str() {
            let trimmed = s.trim();
            if !trimmed.is_empty() && trimmed.len() < 256 {
                ledger.grounded_refs.insert(trimmed.to_string());
            }
        }
    };
    match tool_name {
        "read" | "lookup" | "search" | "outline" | "get_file_outline" | "word" => {
            if let Some(obj) = args.as_object() {
                for key in [
                    "path", "file", "folder", "query", "pattern", "symbol", "word",
                ] {
                    if let Some(value) = obj.get(key) {
                        harvest(value);
                    }
                }
            }
        }
        "bundle" => {
            if let Some(ops) = args.get("ops").and_then(|v| v.as_array()) {
                for op in ops {
                    if let Some(nested) = op.get("args") {
                        record_grounded_refs(ledger, "bundle_op", nested);
                    }
                }
            }
        }
        "bundle_op" => {
            if let Some(obj) = args.as_object() {
                for key in [
                    "path", "file", "folder", "query", "pattern", "symbol", "word",
                ] {
                    if let Some(value) = obj.get(key) {
                        harvest(value);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Convert a `TaskLedger` to a `VmValue` for exposure to Harn code that
/// wants to inspect ledger state (e.g. pipeline post-turn callbacks).
///
/// Currently unused — the agent-loop result path serializes via
/// `serde_json::to_value(&task_ledger)` and then `json_to_vm_value`,
/// which takes a different route. Kept as scaffolding for a future
/// `ledger_inspect()` builtin; delete if it hasn't been wired up by
/// the next minor release.
#[allow(dead_code)]
pub(crate) fn ledger_to_vm_value(ledger: &TaskLedger) -> VmValue {
    use std::collections::BTreeMap;
    let mut root = BTreeMap::new();
    root.insert(
        "root_task".to_string(),
        VmValue::String(Rc::from(ledger.root_task.as_str())),
    );
    let deliverables: Vec<VmValue> = ledger
        .deliverables
        .iter()
        .map(|d| {
            let mut map = BTreeMap::new();
            map.insert("id".to_string(), VmValue::String(Rc::from(d.id.as_str())));
            map.insert(
                "text".to_string(),
                VmValue::String(Rc::from(d.text.as_str())),
            );
            map.insert(
                "status".to_string(),
                VmValue::String(Rc::from(d.status.as_str())),
            );
            if let Some(note) = d.note.as_ref() {
                map.insert("note".to_string(), VmValue::String(Rc::from(note.as_str())));
            }
            VmValue::Dict(Rc::new(map))
        })
        .collect();
    root.insert(
        "deliverables".to_string(),
        VmValue::List(Rc::new(deliverables)),
    );
    root.insert(
        "rationale".to_string(),
        VmValue::String(Rc::from(ledger.rationale.as_str())),
    );
    let observations: Vec<VmValue> = ledger
        .observations
        .iter()
        .map(|o| VmValue::String(Rc::from(o.as_str())))
        .collect();
    root.insert(
        "observations".to_string(),
        VmValue::List(Rc::new(observations)),
    );
    let grounded: Vec<VmValue> = ledger
        .grounded_refs
        .iter()
        .map(|s| VmValue::String(Rc::from(s.as_str())))
        .collect();
    root.insert(
        "grounded_refs".to_string(),
        VmValue::List(Rc::new(grounded)),
    );
    root.insert(
        "blocking_count".to_string(),
        VmValue::Int(ledger.blocking_count() as i64),
    );
    VmValue::Dict(Rc::new(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_mark_flow_gates_done_until_resolved() {
        let mut ledger = TaskLedger {
            root_task: "write tests for FooService".to_string(),
            ..Default::default()
        };
        ledger
            .apply(&serde_json::json!({
                "action": "add",
                "text": "scaffold tests/unit/foo_test.py",
            }))
            .unwrap();
        ledger
            .apply(&serde_json::json!({
                "action": "add",
                "text": "implement five test cases",
            }))
            .unwrap();
        assert!(ledger.gates_done());
        assert_eq!(ledger.blocking_count(), 2);

        ledger
            .apply(&serde_json::json!({
                "action": "mark",
                "id": "deliverable-1",
                "status": "done",
            }))
            .unwrap();
        assert_eq!(ledger.blocking_count(), 1);
        assert!(ledger.gates_done());

        ledger
            .apply(&serde_json::json!({
                "action": "mark",
                "id": "deliverable-2",
                "status": "done",
            }))
            .unwrap();
        assert_eq!(ledger.blocking_count(), 0);
        assert!(!ledger.gates_done());
    }

    #[test]
    fn dropped_satisfies_the_gate_with_note() {
        let mut ledger = TaskLedger::default();
        ledger
            .apply(&serde_json::json!({
                "action": "add",
                "text": "update README",
            }))
            .unwrap();
        let err = ledger.apply(&serde_json::json!({
            "action": "mark",
            "id": "deliverable-1",
            "status": "dropped",
        }));
        assert!(err.is_err(), "dropped without note should be rejected");
        ledger
            .apply(&serde_json::json!({
                "action": "mark",
                "id": "deliverable-1",
                "status": "dropped",
                "note": "user confirmed README not in scope",
            }))
            .unwrap();
        assert!(!ledger.gates_done());
    }

    #[test]
    fn seed_plan_cannot_overwrite_existing_deliverables() {
        let mut ledger = TaskLedger::default();
        ledger
            .apply(&serde_json::json!({
                "action": "seed_plan",
                "deliverables": ["task A", "task B"],
            }))
            .unwrap();
        assert_eq!(ledger.deliverables.len(), 2);
        let err = ledger.apply(&serde_json::json!({
            "action": "seed_plan",
            "deliverables": ["task C"],
        }));
        assert!(err.is_err(), "seed_plan must not clobber");
    }

    #[test]
    fn render_for_prompt_is_empty_when_ledger_is_empty() {
        let ledger = TaskLedger::default();
        assert_eq!(ledger.render_for_prompt(), "");
    }

    #[test]
    fn render_for_prompt_includes_all_fields() {
        let mut ledger = TaskLedger {
            root_task: "refactor auth middleware".to_string(),
            ..Default::default()
        };
        ledger
            .apply(&serde_json::json!({
                "action": "add",
                "text": "extract middleware interface",
            }))
            .unwrap();
        ledger
            .apply(&serde_json::json!({
                "action": "rationale",
                "text": "done when auth paths share one interface and tests pass",
            }))
            .unwrap();
        ledger
            .apply(&serde_json::json!({
                "action": "note",
                "text": "existing middleware lives in src/auth/",
            }))
            .unwrap();
        let rendered = ledger.render_for_prompt();
        assert!(rendered.contains("refactor auth middleware"));
        assert!(rendered.contains("extract middleware interface"));
        assert!(rendered.contains("done when auth paths share one interface"));
        assert!(rendered.contains("existing middleware lives in src/auth/"));
    }

    #[test]
    fn grounded_refs_collected_from_read_and_search_tools() {
        let mut ledger = TaskLedger::default();
        record_grounded_refs(
            &mut ledger,
            "read",
            &serde_json::json!({"path": "src/lib.rs"}),
        );
        record_grounded_refs(
            &mut ledger,
            "search",
            &serde_json::json!({"pattern": "FooService", "file_glob": "src/**/*.py"}),
        );
        record_grounded_refs(
            &mut ledger,
            "bundle",
            &serde_json::json!({
                "ops": [
                    {"tool": "read", "args": {"path": "src/service.py"}},
                    {"tool": "search", "args": {"pattern": "handle_request"}},
                ]
            }),
        );
        assert!(ledger.grounded_refs.contains("src/lib.rs"));
        assert!(ledger.grounded_refs.contains("FooService"));
        assert!(ledger.grounded_refs.contains("src/service.py"));
        assert!(ledger.grounded_refs.contains("handle_request"));
    }
}
