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
    match options.get("session_id") {
        None | Some(VmValue::Nil) => {}
        Some(VmValue::String(explicit))
            if explicit.trim().is_empty() || explicit.as_str() == session_id.as_str() => {}
        Some(VmValue::String(_)) => {
            return Err(VmError::Runtime(
                "with_run: options.session_id must match session_id".into(),
            ));
        }
        Some(_) => {
            return Err(VmError::Runtime(
                "with_run: options.session_id must be a string or nil".into(),
            ));
        }
    }
    let body = match args.get(2) {
        Some(VmValue::Closure(body)) => body.clone(),
        _ => return Err(VmError::Runtime("with_run: body must be a closure".into())),
    };
    if crate::agent_sessions::has_journal(&session_id) {
        return Err(VmError::Runtime(
            "with_run: session already owns a run".into(),
        ));
    }
    let initialized = super::live::initialize(
        &session_id,
        &options,
        None,
        ctx.execution_id(),
        ctx.task_id(),
        &ctx,
    )
    .await?;
    prepare_run(
        &session_id,
        initialized.has_canonical_history,
        crate::stdlib::session_store::canonical_store_state_dir(Some(&options))?,
    )?;
    let mut child = ctx.child_vm();
    let result = child.call_closure_pub(&body, &[]).await;
    ctx.forward_output(&child.take_output());
    if with_run_preparation(&session_id, |preparation| !preparation.adopted) == Some(true) {
        let (status, reason) = match &result {
            Ok(_) => ("completed", "preparation_completed"),
            Err(error) if crate::cancellation::is_cancellation(error) => ("cancelled", "cancelled"),
            Err(_) => ("failed", "preparation_failed"),
        };
        let _ = super::live::flush_init_terminal(&session_id, status, reason, Some(&ctx)).await?;
        if initialized.owns_session {
            crate::agent_sessions::close(&session_id);
        }
    }
    result
}

pub(crate) fn register(vm: &mut Vm) {
    harn_vm_core::builtin_support::register_module_builtins(vm, module_path!());
}

fn with_run_preparation<R>(id: &str, f: impl FnOnce(&mut super::RunPreparation) -> R) -> Option<R> {
    crate::agent_sessions::with_live_journal(id, |journal| journal.preparation.as_mut().map(f))
        .flatten()
}

pub(crate) fn with_initialization_accounting<R>(
    id: &str,
    f: impl FnOnce(&mut super::InitializationAccounting) -> R,
) -> Option<R> {
    crate::agent_sessions::with_live_journal(id, |journal| {
        journal.initialization_accounting.as_mut().map(f)
    })
    .flatten()
}

fn prepare_run(
    id: &str,
    has_canonical_history: bool,
    state_dir: crate::stdlib::session_store::SessionStoreDir,
) -> Result<(), VmError> {
    crate::agent_sessions::with_live_journal(id, |journal| {
        journal.preparation = Some(super::RunPreparation {
            adopted: false,
            has_canonical_history,
            state_dir,
        });
    })
    .ok_or_else(|| VmError::Runtime("run preparation has no active journal".into()))
}

pub(crate) fn adopt_prepared_run(
    id: &str,
    execution_id: &str,
    task_id: &str,
    options: &crate::value::DictMap,
) -> Result<Option<(String, bool, bool)>, VmError> {
    crate::agent_sessions::with_live_journal(id, |journal| {
        let Some(preparation) = journal.preparation.as_ref() else {
            return Ok(None);
        };
        if journal.execution_id() != Some(execution_id) || journal.task_id() != Some(task_id) {
            return Err(VmError::Runtime(
                "prepared run belongs to a different execution task".into(),
            ));
        }
        if preparation.adopted {
            return Err(VmError::Runtime(
                "prepared run already adopted by an agent loop".into(),
            ));
        }
        if let Some(VmValue::String(requested)) = options.get("run_id") {
            if !requested.trim().is_empty() && requested.as_str() != journal.run_id() {
                return Err(VmError::Runtime(
                    "agent loop run_id differs from its prepared run".into(),
                ));
            }
        }
        if preparation.state_dir
            != crate::stdlib::session_store::canonical_store_state_dir(Some(options))?
        {
            return Err(VmError::Runtime(
                "agent loop store root differs from its prepared run".into(),
            ));
        }
        journal.set_workflow_learning_eligibility(options)?;
        let preparation = journal.preparation.as_mut().expect("validated preparation");
        preparation.adopted = true;
        let has_canonical_history = preparation.has_canonical_history;
        Ok(Some((
            journal.run_id().to_string(),
            has_canonical_history,
            journal.owns_session(),
        )))
    })
    .unwrap_or(Ok(None))
}

pub(crate) fn take_initialization_accounting(
    id: &str,
) -> Result<super::InitializationAccounting, VmError> {
    crate::agent_sessions::with_live_journal(id, |journal| {
        let accounting = journal.initialization_accounting.take().ok_or_else(|| {
            VmError::Runtime("agent session initialization has no usage owner".into())
        })?;
        journal.preparation = None;
        Ok(accounting)
    })
    .unwrap_or_else(|| Err(VmError::Runtime("agent session has no live journal".into())))
}
