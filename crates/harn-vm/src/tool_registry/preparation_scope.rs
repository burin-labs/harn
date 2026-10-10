//! Private invocation state shared by dispatch and child interpreters.

use std::future::Future;
use std::sync::Arc;

use harn_builtin_meta::{BuiltinContract, BuiltinExposure, EffectAccess, EffectKind};
use serde_json::Value;

use crate::value::{ErrorCategory, VmClosure, VmError};

tokio::task_local! {
    static PREPARING: ();
    static INVOCATION: Option<Arc<PreparedInvocation>>;
}

pub(crate) struct PreparedInvocation {
    pub(crate) name: String,
    pub(crate) session_id: String,
    pub(crate) arguments: Value,
    pub(crate) facts: Value,
    pub(crate) prepare: Arc<VmClosure>,
}

impl PreparedInvocation {
    pub(crate) fn operation(&self) -> &Value {
        &self.facts["operation"]
    }
}

/// Mandatory even when ordinary policy or reviewer exemptions allow a call.
pub(crate) fn enforce_contract(
    name: &str,
    contract: Option<&BuiltinContract>,
) -> Result<(), VmError> {
    if PREPARING.try_with(|()| true).unwrap_or(false) {
        let permitted = contract.is_some_and(|contract| {
            let pure = matches!(
                contract.exposure,
                BuiltinExposure::PureGlobal | BuiltinExposure::StdlibInternal
            );
            (pure || !contract.effects.is_empty())
                && contract.effects.iter().all(|effect| {
                    effect.access == EffectAccess::Read
                        && matches!(
                            effect.kind,
                            EffectKind::Fs | EffectKind::Env | EffectKind::State | EffectKind::Host
                        )
                })
        });
        if !permitted {
            return Err(VmError::CategorizedError {
                message: format!("{name} is not a read-only invocation preparation operation"),
                category: ErrorCategory::ToolRejected,
            });
        }
    }
    Ok(())
}

pub(crate) async fn preparation<F: Future>(future: F) -> F::Output {
    INVOCATION.scope(None, PREPARING.scope((), future)).await
}

pub(crate) async fn scope<F: Future>(
    invocation: Option<Arc<PreparedInvocation>>,
    future: F,
) -> F::Output {
    INVOCATION.scope(invocation, future).await
}

pub(crate) fn current_invocation() -> Option<Arc<PreparedInvocation>> {
    INVOCATION.try_with(Clone::clone).ok().flatten()
}

/// Capture synchronously before the parent's scope is swapped out. Separate
/// sessions carry restrictions but never another invocation's authority.
pub(crate) fn scope_subtask<F: Future>(
    future: F,
    inherit_invocation: bool,
) -> impl Future<Output = F::Output> {
    let preparing = PREPARING.try_with(|()| true).unwrap_or(false);
    let invocation = inherit_invocation.then(current_invocation).flatten();
    async move {
        scope(invocation, async move {
            if preparing {
                PREPARING.scope((), future).await
            } else {
                future.await
            }
        })
        .await
    }
}
