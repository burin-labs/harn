use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};

harn_vm_core::stdlib_module_installer!(Pure, pub(crate) fn register_channel_builtins);

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "emit_channel(name: string, payload: any, options?: dict) -> dict",
    kind = "async",
    category = "channels"
)]
async fn emit_channel_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    crate::channels::emit_channel_from_vm(Some(&ctx), args).await
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "channel_events(name: string, options?: dict) -> list",
    kind = "async",
    category = "channels"
)]
async fn channel_events_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    crate::channels::channel_events_from_vm(Some(&ctx), args).await
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "channel_subscribe(name: string, options?: dict) -> stream",
    kind = "async",
    category = "channels"
)]
async fn channel_subscribe_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    crate::channels::channel_subscribe_from_vm(Some(&ctx), args).await
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "channel_consumer_cursor(name: string, consumer_id: string, options?: dict) -> int?",
    kind = "async",
    category = "channels"
)]
async fn channel_consumer_cursor_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    crate::channels::channel_consumer_cursor_from_vm(Some(&ctx), args).await
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "channel_ack(name: string, consumer_id: string, cursor: int, options?: dict) -> dict",
    kind = "async",
    category = "channels"
)]
async fn channel_ack_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    crate::channels::channel_ack_from_vm(Some(&ctx), args).await
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "flush_trigger_aggregations() -> nil",
    kind = "async",
    category = "channels"
)]
async fn flush_trigger_aggregations_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    _args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    // CH-04 (#1875): explicit flush so tests can deterministically exercise
    // window-expire semantics together with `advance_time(ms)`.
    crate::channels::flush_expired_aggregations_inner(Some(&ctx)).await;
    Ok(VmValue::Nil)
}
