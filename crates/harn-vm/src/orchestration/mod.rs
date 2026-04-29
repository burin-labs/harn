use std::path::PathBuf;
use std::{cell::RefCell, thread_local};

use serde::{Deserialize, Serialize};

use crate::llm::vm_value_to_json;
use crate::value::{VmError, VmValue};

pub(crate) fn now_rfc3339() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{ts}")
}

pub(crate) fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::now_v7())
}

pub(crate) fn default_run_dir() -> PathBuf {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::runtime_paths::run_root(&base)
}

mod hooks;
pub use hooks::*;

mod command_policy;
pub use command_policy::*;

mod compaction;
pub use compaction::*;

mod artifacts;
pub use artifacts::*;

mod assemble;
pub use assemble::*;

mod handoffs;
pub use handoffs::*;

mod friction;
pub use friction::*;

mod crystallize;
pub use crystallize::*;

mod policy;
pub use policy::*;

mod workflow;
pub use workflow::*;

mod records;
pub use records::*;

thread_local! {
    static CURRENT_MUTATION_SESSION: RefCell<Option<MutationSessionRecord>> = const { RefCell::new(None) };
    /// Workflow-level skill context, installed by `workflow_execute` so
    /// every per-node agent loop constructed inside `execute_stage_node`
    /// can pick up the same `skills:` / `skill_match:` registry without
    /// threading a new parameter through every helper. Cleared on
    /// workflow exit (success or error) by `WorkflowSkillContextGuard`.
    static CURRENT_WORKFLOW_SKILL_CONTEXT: RefCell<Option<WorkflowSkillContext>> = const { RefCell::new(None) };
}

/// Skill wiring threaded from `workflow_execute` into the per-stage
/// agent loops via thread-local context. `VmValue` wraps `Rc` and is
/// not `Send`, so we store it in a thread-local rather than a mutex —
/// the workflow runner pins itself to one task via `LocalSet`, so
/// every stage observes the same context.
#[derive(Clone, Default)]
pub struct WorkflowSkillContext {
    pub registry: Option<VmValue>,
    pub match_config: Option<VmValue>,
}

pub fn install_workflow_skill_context(context: Option<WorkflowSkillContext>) {
    CURRENT_WORKFLOW_SKILL_CONTEXT.with(|slot| {
        *slot.borrow_mut() = context;
    });
}

pub fn current_workflow_skill_context() -> Option<WorkflowSkillContext> {
    CURRENT_WORKFLOW_SKILL_CONTEXT.with(|slot| slot.borrow().clone())
}

/// RAII guard that clears the workflow skill context on drop. Paired
/// with `install_workflow_skill_context` at the top of `execute_workflow`
/// so the context never leaks past a workflow's scope.
pub struct WorkflowSkillContextGuard;

impl Drop for WorkflowSkillContextGuard {
    fn drop(&mut self) {
        install_workflow_skill_context(None);
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct MutationSessionRecord {
    pub session_id: String,
    pub parent_session_id: Option<String>,
    pub run_id: Option<String>,
    pub worker_id: Option<String>,
    pub execution_kind: Option<String>,
    pub mutation_scope: String,
    /// Declarative per-tool approval policy for this session. When `None`,
    /// no policy-driven approval is requested; the session update stream
    /// remains the only host-observable surface for tool dispatch.
    pub approval_policy: Option<ToolApprovalPolicy>,
}

impl MutationSessionRecord {
    pub fn normalize(mut self) -> Self {
        if self.session_id.is_empty() {
            self.session_id = new_id("session");
        }
        if self.mutation_scope.is_empty() {
            self.mutation_scope = "read_only".to_string();
        }
        self
    }
}

pub fn install_current_mutation_session(session: Option<MutationSessionRecord>) {
    CURRENT_MUTATION_SESSION.with(|slot| {
        *slot.borrow_mut() = session.map(MutationSessionRecord::normalize);
    });
}

pub fn current_mutation_session() -> Option<MutationSessionRecord> {
    CURRENT_MUTATION_SESSION.with(|slot| slot.borrow().clone())
}
pub(crate) fn parse_json_payload<T: for<'de> Deserialize<'de>>(
    json: serde_json::Value,
    label: &str,
) -> Result<T, VmError> {
    let payload = json.to_string();
    let mut deserializer = serde_json::Deserializer::from_str(&payload);
    let mut tracker = serde_path_to_error::Track::new();
    let path_deserializer = serde_path_to_error::Deserializer::new(&mut deserializer, &mut tracker);
    T::deserialize(path_deserializer).map_err(|error| {
        let snippet = if payload.len() > 600 {
            format!("{}...", &payload[..600])
        } else {
            payload.clone()
        };
        VmError::Runtime(format!(
            "{label} parse error at {}: {} | payload={}",
            tracker.path(),
            error,
            snippet
        ))
    })
}

pub(crate) fn parse_json_value<T: for<'de> Deserialize<'de>>(
    value: &VmValue,
) -> Result<T, VmError> {
    parse_json_payload(vm_value_to_json(value), "orchestration")
}

#[cfg(test)]
mod tests;
