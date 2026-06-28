use crate::orchestration::MutationSessionRecord;
use crate::value::{VmError, VmValue};

pub(super) fn parse_worker_audit(
    dict: &crate::value::DictMap,
) -> Result<MutationSessionRecord, VmError> {
    let audit_value = dict
        .get("audit")
        .cloned()
        .unwrap_or_else(|| VmValue::dict_map(Default::default()));
    let parent_session = crate::orchestration::current_mutation_session();
    let mut audit: MutationSessionRecord =
        serde_json::from_value(crate::llm::vm_value_to_json(&audit_value))
            .map_err(|e| VmError::Runtime(format!("worker audit parse error: {e}")))?;
    if audit.parent_session_id.is_none() {
        audit.parent_session_id = parent_session
            .as_ref()
            .map(|session| session.session_id.clone());
    }
    if audit.run_id.is_none() {
        audit.run_id = parent_session
            .as_ref()
            .and_then(|session| session.run_id.clone());
    }
    if audit.execution_kind.is_none() {
        audit.execution_kind = Some("worker".to_string());
    }
    if audit.mutation_scope.is_empty() {
        audit.mutation_scope = parent_session
            .as_ref()
            .map(|session| session.mutation_scope.clone())
            .unwrap_or_else(|| "read_only".to_string());
    }
    if audit.approval_policy.is_none() {
        // A top-level agent_loop installs no mutation session, so
        // `parent_session.approval_policy` is None even under `--approve auto`.
        // Fall back to the live approval stack so a background worker inherits
        // the parent's approval policy (auto-approve included) — otherwise its
        // writes hit the host approval gate with no policy and are denied. This
        // mirrors the delegated-stage worker path (execution.rs).
        audit.approval_policy = parent_session
            .as_ref()
            .and_then(|session| session.approval_policy.clone())
            .or_else(crate::orchestration::current_approval_policy);
    }
    Ok(audit.normalize())
}

pub(in super::super) fn inherited_worker_audit(execution_kind: &str) -> MutationSessionRecord {
    let parent_session = crate::orchestration::current_mutation_session();
    MutationSessionRecord {
        parent_session_id: parent_session
            .as_ref()
            .map(|session| session.session_id.clone()),
        run_id: parent_session
            .as_ref()
            .and_then(|session| session.run_id.clone()),
        execution_kind: Some(execution_kind.to_string()),
        mutation_scope: parent_session
            .as_ref()
            .map(|session| session.mutation_scope.clone())
            .unwrap_or_else(|| "read_only".to_string()),
        // See parse_worker_audit: fall back to the live approval stack so a
        // background sub-agent (agent_fanout child) inherits the parent's
        // approval policy / auto-approve instead of defaulting to a denied
        // permission lane.
        approval_policy: parent_session
            .as_ref()
            .and_then(|session| session.approval_policy.clone())
            .or_else(crate::orchestration::current_approval_policy),
        ..Default::default()
    }
    .normalize()
}
