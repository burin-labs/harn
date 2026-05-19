use crate::vm::Vm;

pub(crate) fn register_channel_builtins(vm: &mut Vm) {
    vm.register_async_builtin("emit_channel", |args| async move {
        crate::channels::emit_channel_from_vm(args).await
    });
    vm.register_async_builtin("channel_events", |args| async move {
        crate::channels::channel_events_from_vm(args).await
    });
}
