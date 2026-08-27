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
    match super::execute_llm_call(ctx, opts, options, None, None).await {
        Ok(value) => Ok(value),
        Err(error) => Err(VmError::Thrown(super::build_llm_error_dict(
            &error, &provider, &model,
        ))),
    }
}
