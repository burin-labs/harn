use crate::value::VmValue;
use crate::vm::Vm;

pub(crate) fn register_channel_builtins(vm: &mut Vm) {
    vm.register_async_builtin("emit_channel", |ctx, args| async move {
        crate::channels::emit_channel_from_vm(Some(&ctx), args).await
    });
    vm.register_async_builtin("channel_events", |ctx, args| async move {
        crate::channels::channel_events_from_vm(Some(&ctx), args).await
    });
    // CH-04 (#1875): explicit flush so tests can deterministically
    // exercise window-expire semantics together with `advance_time(ms)`.
    // Production code may call this from a periodic task; the implicit
    // sweep in `emit_channel(...)` already covers the common case.
    vm.register_async_builtin("flush_trigger_aggregations", |ctx, _args| async move {
        crate::channels::flush_expired_aggregations_inner(Some(&ctx)).await;
        Ok(VmValue::Nil)
    });
}
