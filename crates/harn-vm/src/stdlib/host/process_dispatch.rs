use serde_json::Value as JsonValue;

use crate::value::{VmError, VmValue};
use crate::vm::AsyncBuiltinCtx;

use super::bridge::HOST_CALL_BRIDGE;
use super::process_exec::{dispatch_process_exec_after_policy, process_exec_argv};

pub(crate) async fn dispatch_process_exec(
    params: &crate::value::DictMap,
    caller: JsonValue,
) -> Result<VmValue, VmError> {
    dispatch_process_exec_with_policy(None, params, caller).await
}

/// Dispatch the sole structured-git form that may cross the generic
/// catastrophic command floor, as recognized by
/// [`is_reviewed_git_push_with_lease`].
///
/// The caller is crate-private and this boundary validates its argv again, so
/// a Harn script cannot relabel arbitrary `process.exec` input as reviewed.
pub(crate) async fn dispatch_reviewed_git_push_with_lease(
    params: &crate::value::DictMap,
    caller: JsonValue,
) -> Result<VmValue, VmError> {
    validate_reviewed_git_push_with_lease(params)?;
    dispatch_process_exec_with_policy_origin(
        None,
        params,
        caller,
        crate::orchestration::CommandDispatchOrigin::ReviewedGitPushWithLease,
    )
    .await
}

pub(super) async fn dispatch_process_exec_with_policy(
    ctx: Option<&AsyncBuiltinCtx>,
    params: &crate::value::DictMap,
    caller: JsonValue,
) -> Result<VmValue, VmError> {
    dispatch_process_exec_with_policy_origin(
        ctx,
        params,
        caller,
        crate::orchestration::CommandDispatchOrigin::ArbitraryProcess,
    )
    .await
}

async fn dispatch_process_exec_with_policy_origin(
    ctx: Option<&AsyncBuiltinCtx>,
    params: &crate::value::DictMap,
    caller: JsonValue,
    origin: crate::orchestration::CommandDispatchOrigin,
) -> Result<VmValue, VmError> {
    let preflight =
        crate::orchestration::run_command_policy_preflight_with_origin(ctx, params, caller, origin)
            .await?;
    let (params, command_policy_context, command_policy_decisions) = match preflight {
        crate::orchestration::CommandPolicyPreflight::Proceed {
            params,
            context,
            decisions,
        } => (params, context, decisions),
        crate::orchestration::CommandPolicyPreflight::Blocked {
            status,
            message,
            context,
            decisions,
        } => {
            return Ok(crate::orchestration::blocked_command_response(
                params, status, &message, context, decisions,
            ));
        }
    };
    if origin == crate::orchestration::CommandDispatchOrigin::ReviewedGitPushWithLease {
        // A command-policy pre-hook may have rewritten the request after its
        // initial validation. Re-check before either a host bridge or the
        // local process path can observe the command.
        validate_reviewed_git_push_with_lease(&params)?;
    }

    let bridge = HOST_CALL_BRIDGE.with(|bridge| bridge.borrow().clone());
    if let Some(bridge) = bridge {
        if let Some(value) = bridge.dispatch("process", "exec", &params).await? {
            let value = restore_wrapped_spawn_error(value)?;
            return crate::orchestration::run_command_policy_postflight_with_ctx(
                ctx,
                &params,
                value,
                command_policy_context,
                command_policy_decisions,
            )
            .await;
        }
    }

    dispatch_process_exec_after_policy(
        ctx,
        &params,
        command_policy_context,
        command_policy_decisions,
    )
    .await
}

fn restore_wrapped_spawn_error(value: VmValue) -> Result<VmValue, VmError> {
    let Some(result) = value.as_dict() else {
        return Ok(value);
    };
    let exit_code = result.get("exit_code").and_then(|value| match value {
        VmValue::Int(value) => i32::try_from(*value).ok(),
        _ => None,
    });
    let stderr = result.get("stderr").and_then(|value| match value {
        VmValue::String(value) => Some(value.as_bytes()),
        _ => None,
    });
    if let (Some(exit_code), Some(stderr)) = (exit_code, stderr) {
        if let Some(error) = crate::process_sandbox::wrapped_spawn_io_error(exit_code, stderr) {
            return Err(crate::value::environment_io_error_thrown(
                &error,
                error.to_string(),
            ));
        }
    }
    Ok(value)
}

/// Recognize the one structured-git form that may cross the catastrophic
/// command floor: `git push [--no-verify] --force-with-lease=<ref>:<oid>
/// <remote> <refspec>`.
///
/// This is the single owner of that shape. The builtin uses it to choose the
/// reviewed dispatch and the dispatch uses it to re-validate afterwards, so a
/// command-policy pre-hook cannot rewrite an argv into something the selector
/// would no longer have accepted.
///
/// `--no-verify` is the only optional flag, and it does not widen what the push
/// may do to the remote: the lease still names the exact OID it is allowed to
/// replace. A pre-push hook is the checkout's policy about the commits it is
/// publishing, not the remote's policy about its refs. Ref plumbing needs both
/// together — deleting a ref under a lease is the canonical case — so accepting
/// only the flagless form sends exactly that operation to the generic floor,
/// where it is denied as a bare force push.
pub(crate) fn is_reviewed_git_push_with_lease(program: &str, args: &[String]) -> bool {
    if program != "git" || args.first().map(String::as_str) != Some("push") {
        return false;
    }
    let rest = match args.get(1) {
        Some(flag) if flag == "--no-verify" => &args[2..],
        _ => &args[1..],
    };
    let [lease, remote, refspec] = rest else {
        return false;
    };
    lease
        .strip_prefix("--force-with-lease=")
        .and_then(|lease| lease.split_once(':'))
        .is_some_and(|(ref_name, expected_oid)| {
            !ref_name.is_empty()
                && matches!(expected_oid.len(), 40 | 64)
                && expected_oid.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        && !remote.starts_with('-')
        && !refspec.starts_with('-')
}

fn validate_reviewed_git_push_with_lease(params: &crate::value::DictMap) -> Result<(), VmError> {
    let (program, args) = process_exec_argv(params)?;
    if is_reviewed_git_push_with_lease(&program, &args) {
        Ok(())
    } else {
        Err(VmError::Runtime(
            "reviewed git dispatch requires `git push [--no-verify] --force-with-lease=<ref>:<oid> <remote> <refspec>`"
                .to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::value::VmValue;

    use super::{is_reviewed_git_push_with_lease, validate_reviewed_git_push_with_lease};

    const OID: &str = "0123456789abcdef0123456789abcdef01234567";

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_string()).collect()
    }

    /// Ref plumbing needs `--no-verify` and a lease together: deleting a ref
    /// under a lease is the canonical case, and `std/git::git_push` emits both
    /// flags for it. Rejecting that argv sent it to the generic command floor,
    /// where it was denied as a bare force push — an error naming neither the
    /// lease nor the hook.
    #[test]
    fn a_leased_ref_plumbing_push_may_also_skip_the_pre_push_hook() {
        let lease = format!("--force-with-lease=refs/heads/attempt:{OID}");
        assert!(is_reviewed_git_push_with_lease(
            "git",
            &args(&[
                "push",
                &lease,
                "origin",
                &format!("{OID}:refs/heads/archived")
            ]),
        ));
        assert!(is_reviewed_git_push_with_lease(
            "git",
            &args(&[
                "push",
                "--no-verify",
                &lease,
                "origin",
                &format!("{OID}:refs/heads/archived"),
            ]),
        ));
    }

    /// `--no-verify` is the only flag the reviewed form tolerates, and only
    /// ahead of the lease. Anything else must fall back to the generic floor.
    #[test]
    fn no_other_flag_may_ride_along_with_the_reviewed_lease() {
        let lease = format!("--force-with-lease=refs/heads/attempt:{OID}");
        for extra in ["--force", "--mirror", "--delete", "-f"] {
            assert!(
                !is_reviewed_git_push_with_lease(
                    "git",
                    &args(&["push", extra, &lease, "origin", "HEAD:main"]),
                ),
                "{extra} must not be accepted before the lease"
            );
            assert!(
                !is_reviewed_git_push_with_lease(
                    "git",
                    &args(&["push", &lease, extra, "origin", "HEAD:main"]),
                ),
                "{extra} must not be accepted after the lease"
            );
        }
        assert!(!is_reviewed_git_push_with_lease(
            "git",
            &args(&["push", "--no-verify", "origin", "HEAD:main"]),
        ));
        assert!(!is_reviewed_git_push_with_lease(
            "hub",
            &args(&["push", &lease, "origin", "HEAD:main"]),
        ));
    }

    #[test]
    fn reviewed_git_dispatch_requires_an_exact_oid_lease() {
        for lease in [
            "--force",
            "--force-with-lease=refs/heads/main",
            "--force-with-lease=refs/heads/main:",
            "--force-with-lease=refs/heads/main:abc123",
        ] {
            let params = crate::value::DictMap::from_iter([
                (
                    crate::value::intern_key("mode"),
                    VmValue::String(arcstr::ArcStr::from("argv")),
                ),
                (
                    crate::value::intern_key("argv"),
                    VmValue::List(Arc::new(vec![
                        VmValue::String(arcstr::ArcStr::from("git")),
                        VmValue::String(arcstr::ArcStr::from("push")),
                        VmValue::String(arcstr::ArcStr::from(lease)),
                        VmValue::String(arcstr::ArcStr::from("origin")),
                        VmValue::String(arcstr::ArcStr::from("HEAD:main")),
                    ])),
                ),
            ]);

            let error = validate_reviewed_git_push_with_lease(&params)
                .expect_err("only an exact force-with-lease argv may use the reviewed dispatch");
            assert!(
                error.to_string().contains("reviewed git dispatch requires"),
                "unexpected error for {lease}: {error}"
            );
        }
    }
}
