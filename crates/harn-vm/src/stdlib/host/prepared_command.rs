use std::{future::Future, pin::Pin};

use crate::{
    orchestration::{CommandInvocation, CommandPolicyParameters},
    value::{DictMap, VmError, VmValue},
};

mod remote;
pub use remote::{
    command_binding_schemas, prepare_remote_command, CommandTransportFuture,
    PreparedCommandTransport,
};

/// A host either declines ownership, cannot bind launch facts, or retains one
/// request. Unsupported binding must never become a VM-local execution.
pub enum HostCommandPreparation {
    Unhandled,
    Unsupported,
    Bound(Box<dyn PreparedHostCommand>),
}

pub type HostCommandPreparationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<HostCommandPreparation, VmError>> + Send + 'a>>;

/// Executor-owned, single-use request. Dropping an unused preparation must not
/// launch anything; dropping its execution future must cancel its child work.
pub trait PreparedHostCommand: Send {
    fn invocation(&self) -> &CommandInvocation;

    fn execute(self: Box<Self>) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + Send>>;
}

pub(super) enum PreparedCommand {
    Local(super::process_exec::PreparedLocalCommand),
    Host {
        params: DictMap,
        command: Box<dyn PreparedHostCommand>,
    },
}

impl CommandPolicyParameters for PreparedCommand {
    fn parameters(&self) -> &DictMap {
        match self {
            Self::Local(command) => command.parameters(),
            Self::Host { params, .. } => params,
        }
    }

    fn invocation(&self) -> Option<serde_json::Value> {
        match self {
            Self::Local(command) => CommandPolicyParameters::invocation(command),
            Self::Host { command, .. } => Some(command.invocation().audit()),
        }
    }

    fn local_filesystem(&self) -> bool {
        matches!(self, Self::Local(_))
    }
}

pub(super) async fn prepare_command(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    mut params: DictMap,
    operation: &'static str,
    mode: super::HostDispatch,
) -> Result<PreparedCommand, VmError> {
    // Ordinary spawn handles belong to the VM's registry. Only an explicitly
    // host-bound capability transfers spawn and handle operations to a host.
    let bridge = if operation == "spawn" && mode == super::HostDispatch::AllowFallback {
        None
    } else {
        super::HOST_CALL_BRIDGE.with(|bridge| bridge.borrow().clone())
    };
    if let Some(bridge) = bridge {
        let trace = super::trace::HostRequestTrace::new(
            ctx.and_then(crate::vm::AsyncBuiltinCtx::host_call_site),
            &params,
        );
        match bridge.prepare_command(operation, &params, &trace).await? {
            HostCommandPreparation::Bound(command) => {
                command.invocation().bind_directory(&mut params);
                return Ok(PreparedCommand::Host { params, command });
            }
            HostCommandPreparation::Unsupported => {
                return Err(VmError::Runtime(format!(
                    "host process.{operation} cannot bind command invocation and directory"
                )));
            }
            HostCommandPreparation::Unhandled => {}
        }
    }
    if mode == super::HostDispatch::RequireAdapter {
        return Err(super::missing_host_adapter("process", operation));
    }
    Ok(PreparedCommand::Local(
        super::process_exec::PreparedLocalCommand::prepare(params, operation)?,
    ))
}
