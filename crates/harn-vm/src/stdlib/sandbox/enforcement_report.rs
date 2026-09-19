//! Live Linux filesystem-confinement receipt for CI.
//!
//! The report and the decision share the same runtime seam. A host without
//! Landlock must prove that an outside write actually succeeds; otherwise
//! `active=false` could be a detector stuck on false. A host with Landlock must
//! prove the inverse. The two branches deliberately cannot both pass.

use std::path::Path;

use crate::orchestration::{
    current_execution_policy, pop_execution_policy, push_execution_policy, CapabilityPolicy,
    SandboxProfile,
};
use crate::security::SessionEnvironment;
use crate::tool_annotations::SideEffectLevel;

use super::{
    active_backend_filesystem_available, active_backend_filesystem_mechanism, active_backend_name,
    command_output, ProcessCommandConfig,
};

struct PolicyGuard;

impl Drop for PolicyGuard {
    fn drop(&mut self) {
        crate::stdlib::process::set_session_environment(None);
        pop_execution_policy();
    }
}

fn enter_worktree_policy(workspace: &Path) -> PolicyGuard {
    push_execution_policy(CapabilityPolicy {
        workspace_roots: vec![workspace.display().to_string()],
        side_effect_level: Some(SideEffectLevel::ProcessExec.as_str().to_string()),
        sandbox_profile: SandboxProfile::Worktree,
        ..CapabilityPolicy::default()
    });
    crate::stdlib::process::set_session_environment(Some(SessionEnvironment::isolated()));
    PolicyGuard
}

#[test]
fn process_filesystem_sandbox_report_matches_live_escape() {
    let root = tempfile::tempdir().expect("probe root");
    let workspace = root.path().join("workspace");
    let outside = root.path().join("outside-write");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let _policy = enter_worktree_policy(&workspace);

    let result = command_output(
        "/usr/bin/touch",
        &[outside.display().to_string()],
        &ProcessCommandConfig {
            cwd: Some(workspace),
            ..ProcessCommandConfig::default()
        },
    );
    let active = active_backend_filesystem_available();
    let outcome = if active {
        assert!(
            result.is_err(),
            "active filesystem sandbox allowed an outside write"
        );
        assert!(
            !outside.exists(),
            "active filesystem sandbox created the outside file"
        );
        "denied"
    } else {
        let output = result.expect("an unavailable best-effort sandbox must run the child");
        assert!(
            output.status.success(),
            "unconfined outside-write control failed for a non-sandbox reason: {output:?}"
        );
        assert!(
            outside.exists(),
            "inactive detector claimed no confinement, but the outside write did not land"
        );
        "allowed"
    };

    // The Unix-socket disposition rides the same receipt because it is the
    // term whose meaning differs most between backends, and a reader who
    // cannot see which shape was applied would have to infer it from the
    // platform. `not_requested` here is a measurement of this probe policy,
    // which asks for no sockets, and not a claim that the grant is inert.
    let unix_sockets = super::unix_socket_enforcement(
        &current_execution_policy().expect("probe policy is pushed"),
    );
    let marker = format!(
        "harn.sandbox_enforcement schema=harn.ci.sandbox_enforcement.v1 backend={} filesystem_mechanism={} active={} outside_write={outcome} unix_sockets={}",
        active_backend_name(),
        active_backend_filesystem_mechanism(),
        active,
        serde_json::to_value(unix_sockets)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".to_string()),
    );
    println!("{marker}");
    if let Ok(path) = std::env::var("HARN_SANDBOX_ENFORCEMENT_RECEIPT") {
        std::fs::write(path, format!("{marker}\n")).expect("write sandbox enforcement receipt");
    }
}
