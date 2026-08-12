//! Embedder `HostCallBridge` seam for canonical `host_call` dispatch.

use std::cell::RefCell;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;

use crate::value::{VmError, VmValue};

use super::turn_cache;

/// Boxed future returned by [`HostCallBridge::dispatch`].
///
/// Prefer this over `async_trait`: production call sites are already async,
/// and a hand-rolled boxed future is the smallest seam that lets
/// `dispatch_host_operation_with_ctx` await a bridge. The future is `Send`
/// because the stdlib `host_call` builtin is registered through the async
/// builtin path, which requires `Send` futures even when embedders pin work
/// to a current-thread `LocalSet`.
pub type HostCallDispatchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<VmValue>, VmError>> + Send + 'a>>;

/// Box a ready host-call result for sync bridge implementations.
pub fn host_call_ready(
    result: Result<Option<VmValue>, VmError>,
) -> HostCallDispatchFuture<'static> {
    Box::pin(async move { result })
}

/// Embedder-supplied bridge for `host_call` ops.
///
/// Embedders (debug adapters, CLIs, IDE hosts) implement this trait to
/// satisfy capability/operation pairs that harn-vm itself doesn't know how
/// to handle. Returning `Ok(None)` means "I don't handle this op — fall
/// through to the built-in fallbacks (env-derived defaults, then the
/// `unsupported operation` error)". `Ok(Some(value))` is the result;
/// `Err(VmError::Thrown(_))` surfaces as a Harn exception.
///
/// `dispatch` returns a `Send` boxed future so network protocol bridges
/// (ACP `host/call`, DAP reverse requests) can await without blocking the
/// caller. Sync bridges can return [`host_call_ready`].
pub trait HostCallBridge: Send + Sync {
    fn dispatch<'a>(
        &'a self,
        capability: &'a str,
        operation: &'a str,
        params: &'a crate::value::DictMap,
    ) -> HostCallDispatchFuture<'a>;

    fn list_tools(&self) -> Result<Option<VmValue>, VmError> {
        Ok(None)
    }

    fn call_tool(&self, _name: &str, _args: &VmValue) -> Result<Option<VmValue>, VmError> {
        Ok(None)
    }
}

thread_local! {
    pub(super) static HOST_CALL_BRIDGE: RefCell<Option<Arc<dyn HostCallBridge>>> =
        const { RefCell::new(None) };
}

/// Install a bridge for the current thread. The bridge is consulted on
/// every `host_call` *after* mock matching but *before* the built-in
/// match arms, so embedders can override anything they like (and equally
/// punt on anything they don't, by returning `Ok(None)`).
pub fn set_host_call_bridge(bridge: Arc<dyn HostCallBridge>) {
    turn_cache::reset();
    HOST_CALL_BRIDGE.with(|b| *b.borrow_mut() = Some(bridge));
}

/// Install a bridge for one lexical host scope and restore the previous bridge on drop.
pub fn install_host_call_bridge(bridge: Arc<dyn HostCallBridge>) -> HostCallBridgeGuard {
    turn_cache::reset();
    let previous = HOST_CALL_BRIDGE.with(|slot| slot.borrow_mut().replace(bridge));
    HostCallBridgeGuard {
        previous,
        _not_send: PhantomData,
    }
}

#[must_use = "dropping the guard restores the previous host-call bridge"]
pub struct HostCallBridgeGuard {
    previous: Option<Arc<dyn HostCallBridge>>,
    // The installed bridge lives in thread-local storage and must be restored
    // on the same thread.
    _not_send: PhantomData<Rc<()>>,
}

impl Drop for HostCallBridgeGuard {
    fn drop(&mut self) {
        turn_cache::reset();
        let previous = self.previous.take();
        HOST_CALL_BRIDGE.with(|slot| *slot.borrow_mut() = previous);
    }
}

/// Remove the current thread's bridge. Idempotent.
pub fn clear_host_call_bridge() {
    turn_cache::reset();
    HOST_CALL_BRIDGE.with(|b| *b.borrow_mut() = None);
}

/// Dispatch `(capability, operation, params)` to the currently-installed
/// `HostCallBridge`, if any. `Some(Ok(_))` means the bridge handled the
/// call; `Some(Err(_))` means it tried but raised; `None` means there is
/// no bridge or the bridge declined this op (returned `Ok(None)`).
///
/// Mirrors the inner block of `dispatch_host_operation` but without the
/// mock-call check or the built-in fallbacks — useful for callers that
/// want to treat the bridge as one of several sinks (e.g. inbound MCP
/// `elicitation/create` requests).
pub async fn dispatch_host_call_bridge(
    capability: &str,
    operation: &str,
    params: &crate::value::DictMap,
) -> Option<Result<VmValue, VmError>> {
    let bridge = HOST_CALL_BRIDGE.with(|b| b.borrow().clone())?;
    match bridge.dispatch(capability, operation, params).await {
        Ok(Some(value)) => Some(Ok(value)),
        Ok(None) => None,
        Err(error) => Some(Err(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::DictMap;

    struct NamedBridge(&'static str);

    impl HostCallBridge for NamedBridge {
        fn dispatch<'a>(
            &'a self,
            _capability: &'a str,
            _operation: &'a str,
            _params: &'a DictMap,
        ) -> HostCallDispatchFuture<'a> {
            host_call_ready(Ok(Some(VmValue::String(self.0.into()))))
        }
    }

    async fn active_name() -> Option<String> {
        dispatch_host_call_bridge("test", "name", &DictMap::new())
            .await?
            .ok()
            .and_then(|value| match value {
                VmValue::String(value) => Some(value.to_string()),
                _ => None,
            })
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lexical_bridge_install_restores_nested_state() {
        clear_host_call_bridge();
        let outer = install_host_call_bridge(Arc::new(NamedBridge("outer")));
        assert_eq!(active_name().await.as_deref(), Some("outer"));
        {
            let _inner = install_host_call_bridge(Arc::new(NamedBridge("inner")));
            assert_eq!(active_name().await.as_deref(), Some("inner"));
        }
        assert_eq!(active_name().await.as_deref(), Some("outer"));
        drop(outer);
        assert_eq!(active_name().await, None);
    }
}
