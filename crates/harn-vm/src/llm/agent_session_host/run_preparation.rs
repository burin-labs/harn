use crate::value::{VmError, VmValue};
use crate::vm::{AsyncBuiltinCtx, Vm};
use harn_builtin_macros::harn_builtin;

/// Start the canonical run before preparatory calls. The first agent loop in
/// the callback adopts this journal and its measured provider usage.
#[harn_builtin(
    exposure = "harness.agent.with_run",
    effects = [],
    sig = "with_run(session_id: string, options: dict, body: closure) -> any",
    kind = "async",
    category = "agent.host",
    doc = "Record preparation and one agent loop as one run. The callback's first loop adopts the run; a callback that returns or fails before that loop receives its own durable terminal. Nested preparation on the same session is refused."
)]
async fn with_run(ctx: AsyncBuiltinCtx, args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(id)) if !id.trim().is_empty() => id.to_string(),
        _ => {
            return Err(VmError::Runtime(
                "with_run: session_id must be non-empty".into(),
            ))
        }
    };
    let options = match args.get(1) {
        Some(VmValue::Dict(options)) => options.as_ref().clone(),
        _ => {
            return Err(VmError::Runtime(
                "with_run: options must be a dictionary".into(),
            ))
        }
    };
    let body = match args.get(2) {
        Some(VmValue::Closure(body)) => body.clone(),
        _ => return Err(VmError::Runtime("with_run: body must be a closure".into())),
    };
    if crate::agent_sessions::has_journal(&session_id) {
        return Err(VmError::Runtime(
            "with_run: session already owns a run".into(),
        ));
    }
    let initialized = super::live_transcript_journal::initialize(
        &session_id,
        &options,
        None,
        ctx.execution_id(),
        ctx.task_id(),
        &ctx,
    )
    .await?;
    crate::agent_sessions::prepare_run(
        &session_id,
        initialized.has_canonical_history,
        crate::stdlib::session_store::canonical_store_state_dir(Some(&options))?,
    )?;
    let frame = crate::agent_sessions::push_current_session(session_id.clone())
        .expect("validated non-empty session id");
    let mut child = ctx.child_vm();
    let result = child.call_closure_pub(&body, &[]).await;
    ctx.forward_output(&child.take_output());
    if crate::agent_sessions::with_run_preparation(&session_id, |preparation| !preparation.adopted)
        == Some(true)
    {
        let (status, reason) = if result.is_ok() {
            ("completed", "preparation_completed")
        } else {
            ("failed", "preparation_failed")
        };
        let terminal = super::live_transcript_journal::flush_init_terminal(
            &session_id,
            status,
            reason,
            Some(&ctx),
        )
        .await;
        crate::agent_sessions::remove_current_session(frame);
        terminal?;
    } else {
        crate::agent_sessions::remove_current_session(frame);
    }
    result
}

pub(super) fn register(vm: &mut Vm) {
    harn_vm_core::builtin_support::register_module_builtins(vm, module_path!());
}
