//! macOS sandbox backend — `/usr/bin/sandbox-exec` wrapper rendered
//! from the active capability set.
//!
//! `sandbox-exec` is technically deprecated but remains the platform
//! mechanism most production macOS sandboxes still rely on; Apple has
//! not shipped a supported successor for non-App-Store binaries. The
//! generated profile is a tight default-deny policy with explicit
//! allow rules for the workspace roots and a small list of
//! system-read directories required to exec common binaries.
//!
//! See `docs/src/sandboxing.md` for the capability → kernel-knob
//! mapping table.

use std::path::Path;
use std::process::Command;

use super::{
    policy_allows_network, policy_allows_workspace_write, process_sandbox_roots, unavailable,
    PrepareOutcome, SandboxBackend,
};
use crate::orchestration::{CapabilityPolicy, SandboxProfile};
use crate::value::VmError;

const SANDBOX_EXEC_PATH: &str = "/usr/bin/sandbox-exec";

pub(super) struct Backend;

impl SandboxBackend for Backend {
    fn name() -> &'static str {
        "macos"
    }

    fn available() -> bool {
        Path::new(SANDBOX_EXEC_PATH).exists()
    }

    fn prepare_std_command(
        program: &str,
        args: &[String],
        _command: &mut Command,
        policy: &CapabilityPolicy,
        profile: SandboxProfile,
    ) -> Result<PrepareOutcome, VmError> {
        wrap_with_sandbox_exec(program, args, policy, profile)
    }

    fn prepare_tokio_command(
        program: &str,
        args: &[String],
        _command: &mut tokio::process::Command,
        policy: &CapabilityPolicy,
        profile: SandboxProfile,
    ) -> Result<PrepareOutcome, VmError> {
        wrap_with_sandbox_exec(program, args, policy, profile)
    }
}

fn wrap_with_sandbox_exec(
    program: &str,
    args: &[String],
    policy: &CapabilityPolicy,
    profile: SandboxProfile,
) -> Result<PrepareOutcome, VmError> {
    if !Path::new(SANDBOX_EXEC_PATH).exists() {
        return unavailable("macOS sandbox-exec is not available", profile);
    }
    let mut wrapped_args = vec![
        "-p".to_string(),
        render_profile(policy),
        "--".to_string(),
        program.to_string(),
    ];
    wrapped_args.extend(args.iter().cloned());
    Ok(PrepareOutcome::WrappedExec {
        wrapper: SANDBOX_EXEC_PATH.to_string(),
        args: wrapped_args,
    })
}

fn render_profile(policy: &CapabilityPolicy) -> String {
    let roots = process_sandbox_roots(policy);
    let mut profile = String::from(
        "(version 1)\n\
         (deny default)\n\
         (allow process*)\n\
         (allow sysctl-read)\n\
         (allow mach-lookup)\n\
         (allow file-read-metadata)\n\
         (allow file-read-data (literal \"/\"))\n\
         (allow file-write* (subpath \"/dev\"))\n\
         (allow file-read* (subpath \"/private/var/select\"))\n\
         (allow file-read* (subpath \"/var/select\"))\n\
         (allow file-read* (subpath \"/Library/Developer\"))\n",
    );
    for root in system_read_roots() {
        profile.push_str(&format!(
            "(allow file-read* (subpath \"{}\"))\n",
            sandbox_profile_escape(root)
        ));
    }
    for root in &roots {
        profile.push_str(&format!(
            "(allow file-read* (subpath \"{}\"))\n",
            sandbox_profile_escape(&root.display().to_string())
        ));
    }
    if policy_allows_workspace_write(policy) {
        profile.push_str(
            "(allow file-write* (subpath \"/tmp\") (subpath \"/private/tmp\") (subpath \"/var/tmp\"))\n",
        );
        for root in roots {
            profile.push_str(&format!(
                "(allow file-write* (subpath \"{}\"))\n",
                sandbox_profile_escape(&root.display().to_string())
            ));
        }
    }
    if policy_allows_network(policy) {
        profile.push_str("(allow network*)\n");
    }
    profile
}

fn system_read_roots() -> &'static [&'static str] {
    &[
        "/bin",
        "/etc",
        "/Library",
        "/opt/homebrew",
        "/private/etc",
        "/System",
        "/usr",
    ]
}

fn sandbox_profile_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn macos_policy_with_workspace_ops(ops: &[&str]) -> CapabilityPolicy {
        CapabilityPolicy {
            tools: Vec::new(),
            capabilities: std::collections::BTreeMap::from([(
                "workspace".to_string(),
                ops.iter().map(|op| op.to_string()).collect(),
            )]),
            workspace_roots: vec!["/tmp/harn-workspace".to_string()],
            side_effect_level: Some("read_only".to_string()),
            recursion_limit: None,
            tool_arg_constraints: Vec::new(),
            tool_annotations: std::collections::BTreeMap::new(),
            sandbox_profile: SandboxProfile::Worktree,
        }
    }

    #[test]
    fn sandbox_profile_does_not_grant_global_file_read() {
        let profile = render_profile(&macos_policy_with_workspace_ops(&["read_text"]));
        assert!(
            !profile.contains("(allow file-read*)\n"),
            "profile must not grant global file reads"
        );
        assert!(
            profile.contains("(allow file-read-data (literal \"/\"))"),
            "profile should permit root-directory reads needed to exec common macOS binaries"
        );
        assert!(
            profile.contains("harn-workspace"),
            "workspace root should be included in scoped read grants: {profile}"
        );
    }

    #[test]
    fn sandbox_profile_allows_tmp_write_only_with_workspace_write() {
        let read_only = render_profile(&macos_policy_with_workspace_ops(&["read_text"]));
        assert!(
            !read_only.contains("(subpath \"/tmp\") (subpath \"/private/tmp\")"),
            "read-only profile must not grant temp writes"
        );

        let writable = render_profile(&macos_policy_with_workspace_ops(&["write_text"]));
        assert!(writable.contains("(subpath \"/tmp\") (subpath \"/private/tmp\")"));
    }
}
