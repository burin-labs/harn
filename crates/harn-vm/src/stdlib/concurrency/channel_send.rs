use super::*;

pub(super) async fn send(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    if args.len() < 2 {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "send: requires channel and value",
        ))));
    }
    if let VmValue::Channel(ch) = &args[0] {
        let vm = current_async_vm(&ctx, "send");
        let target = channel_target(ch);
        let mut closed_rx = ch.subscribe_closed();
        if ch.is_closed() || *closed_rx.borrow() {
            return Err(channel_closed_error("send", ch.name.as_ref()));
        }
        let mut val = args[1].clone();
        loop {
            val = match ch.sender.try_send(val) {
                Ok(()) => {
                    vm.wait_for_graph.notify_channel_send(&target);
                    return Ok(VmValue::Bool(true));
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(val)) => val,
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    return Err(channel_closed_error("send", ch.name.as_ref()));
                }
            };
            // Another receiver may free capacity between try_send and wait
            // registration. Retry the send instead of blocking without a wait
            // record if the graph observes that ready state.
            let Some(_wait) = channel_send_wait(&vm, target.clone())? else {
                tokio::task::yield_now().await;
                continue;
            };
            return tokio::select! {
                biased;
                _ = closed_rx.changed() => Err(channel_closed_error("send", ch.name.as_ref())),
                result = ch.sender.send(val) => {
                    match result {
                        Ok(()) => {
                            vm.wait_for_graph.notify_channel_send(&target);
                            Ok(VmValue::Bool(true))
                        }
                        Err(_) => Err(channel_closed_error("send", ch.name.as_ref())),
                    }
                }
            };
        }
    } else {
        Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "send: first argument must be a channel",
        ))))
    }
}
