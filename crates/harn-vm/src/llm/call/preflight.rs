//! One preflight boundary for throwing and safe LLM calls.

use crate::llm::helpers;
use crate::value::{VmError, VmValue};

#[derive(Clone, Copy)]
pub(super) enum ErrorSurface {
    Throwing,
    Safe,
}

/// Resolve caller options, establish the render context, and dispatch through
/// the canonical call path. Only local preflight errors vary by public surface;
/// provider failures always use the shared structured taxonomy.
pub(super) async fn execute(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    args: Vec<VmValue>,
    surface: ErrorSurface,
) -> Result<VmValue, VmError> {
    let options = args.get(2).and_then(VmValue::as_dict).cloned();
    let opts = match surface {
        ErrorSurface::Throwing => helpers::prepare_llm_options(&args).await?,
        ErrorSurface::Safe => helpers::prepare_llm_options_safe(&args).await?,
    };
    let provider = opts.provider.clone();
    let model = opts.model.clone();

    let _render_guard = crate::stdlib::template::LlmRenderContextGuard::enter(
        crate::stdlib::template::LlmRenderContext::resolve(&provider, &model),
    );
    // One measurement scope per logical call, whichever surface entered it.
    // A thrown terminal carries the count the same way `agent_loop`'s does,
    // so a consumer can tell a refusal before dispatch from a failure after.
    let ledger = crate::llm::provider_dispatch::ProviderDispatchLedger::default();
    let outcome = crate::llm::provider_dispatch::with_provider_dispatch_ledger(
        ledger.clone(),
        super::execute_llm_call(ctx, opts, options, None, None),
    )
    .await;
    match outcome {
        Ok(value) => Ok(value),
        Err(error) => Err(crate::llm::provider_dispatch::stamp_thrown_terminal(
            VmError::Thrown(super::build_llm_error_dict(&error, &provider, &model)),
            &ledger,
        )),
    }
}
