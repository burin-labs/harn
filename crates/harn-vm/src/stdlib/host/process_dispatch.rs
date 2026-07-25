use serde_json::Value as JsonValue;

use crate::value::{VmError, VmValue};
use crate::vm::AsyncBuiltinCtx;

use super::process_exec::{dispatch_process_exec_after_policy, process_exec_argv};
use super::HOST_CALL_BRIDGE;

pub(crate) async fn dispatch_process_exec(
    params: &crate::value::DictMap,
    caller: JsonValue,
) -> Result<VmValue, VmError> {
    dispatch_process_exec_with_policy(None, params, caller).await
}

/// Dispatch the sole structured-git form that may cross the generic
/// catastrophic command floor: `git push --force-with-lease=<ref>:<oid>`.
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
        if let Some(value) = bridge.dispatch("process", "exec", &params)? {
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

fn validate_reviewed_git_push_with_lease(params: &crate::value::DictMap) -> Result<(), VmError> {
    let (program, args) = process_exec_argv(params)?;
    let valid = program == "git"
        && args.len() == 4
        && args[0] == "push"
        && args[1]
            .strip_prefix("--force-with-lease=")
            .and_then(|lease| lease.split_once(':'))
            .is_some_and(|(ref_name, expected_oid)| {
                !ref_name.is_empty()
                    && matches!(expected_oid.len(), 40 | 64)
                    && expected_oid.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        && !args[2].starts_with('-')
        && !args[3].starts_with('-');
    if valid {
        Ok(())
    } else {
        Err(VmError::Runtime(
            "reviewed git dispatch requires `git push --force-with-lease=<ref>:<oid> <remote> <refspec>`"
                .to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::value::VmValue;

    use super::validate_reviewed_git_push_with_lease;

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
