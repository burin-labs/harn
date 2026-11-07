use std::sync::Arc;

use harn_vm::stdlib::host::trace::HostRequestTrace;
use harn_vm::stdlib::host::{CommandTransportFuture, PreparedCommandTransport};
use serde_json::{json, Value};

use super::AcpBridge;

pub(super) struct CommandTransport(pub(super) Arc<AcpBridge>);

impl PreparedCommandTransport for CommandTransport {
    fn call<'a>(
        &'a self,
        operation: &'a str,
        args: Value,
        trace: &'a HostRequestTrace,
        cleanup: bool,
    ) -> CommandTransportFuture<'a> {
        Box::pin(async move {
            let params = json!({
                "sessionId": self.0.session_id,
                "name": format!("process.{operation}"),
                "args": args,
                "_meta": {"harn": {"requestTrace": trace}},
            });
            if cleanup {
                self.0.call_client_for_cleanup("host/call", params).await
            } else {
                self.0.call_client("host/call", params).await
            }
        })
    }
}
