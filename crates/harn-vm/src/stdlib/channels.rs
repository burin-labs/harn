use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_channel_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(
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

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &EMIT_CHANNEL_IMPL_DEF,
    &CHANNEL_EVENTS_IMPL_DEF,
    &CHANNEL_SUBSCRIBE_IMPL_DEF,
    &FLUSH_TRIGGER_AGGREGATIONS_IMPL_DEF,
];
