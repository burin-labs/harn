use crate::stdlib::host::{
    trace::HostRequestTrace, HostCommandPreparation, HostCommandPreparationFuture,
    PreparedHostCommand,
};
use crate::{HostCallBridge, HostCallDispatchFuture, VmError, VmValue};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub(crate) struct CapabilityBridge(pub(crate) Arc<Mutex<Vec<serde_json::Value>>>);

impl CapabilityBridge {
    fn record(
        &self,
        capability: &str,
        operation: &str,
        params: &crate::value::DictMap,
        phase: &str,
    ) {
        self.0.lock().unwrap().push(serde_json::json!({
            "name": format!("{capability}.{operation}"),
            "args": crate::llm::vm_value_to_json(&VmValue::dict(params.clone())),
            "phase": phase,
        }));
    }
}

impl HostCallBridge for CapabilityBridge {
    fn prepare_command<'a>(
        &'a self,
        operation: &'a str,
        params: &'a crate::value::DictMap,
        _trace: &'a HostRequestTrace,
    ) -> HostCommandPreparationFuture<'a> {
        Box::pin(async move {
            self.record("process", operation, params, "prepare");
            Ok(HostCommandPreparation::Unhandled)
        })
    }

    fn dispatch<'a>(
        &'a self,
        capability: &'a str,
        operation: &'a str,
        params: &'a crate::value::DictMap,
    ) -> HostCallDispatchFuture<'a> {
        Box::pin(async move {
            self.record(capability, operation, params, "dispatch");
            Ok(((capability, operation) == ("session_inputs", "request"))
                .then(|| VmValue::string("input requested")))
        })
    }
}

pub(crate) struct CountingBridge {
    pub(crate) calls: Arc<AtomicUsize>,
    pub(crate) preparations: Arc<AtomicUsize>,
}

struct BoundProcess {
    calls: Arc<AtomicUsize>,
    invocation: crate::orchestration::CommandInvocation,
}

impl PreparedHostCommand for BoundProcess {
    fn invocation(&self) -> &crate::orchestration::CommandInvocation {
        &self.invocation
    }

    fn execute(
        self: Box<Self>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<VmValue, VmError>> + Send>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(crate::json_to_vm_value(&serde_json::json!({
                "exit_code": 0, "status": "completed", "stdout": "allowed", "stderr": ""
            })))
        })
    }
}

impl HostCallBridge for CountingBridge {
    fn prepare_command<'a>(
        &'a self,
        operation: &'a str,
        params: &'a crate::value::DictMap,
        _trace: &'a HostRequestTrace,
    ) -> HostCommandPreparationFuture<'a> {
        Box::pin(async move {
            assert_eq!(operation, "exec");
            let shell = crate::shells::resolve_invocation_from_vm_params(params)
                .map_err(VmError::Runtime)?;
            let invocation = crate::orchestration::CommandInvocation::new(
                std::iter::once(shell.program).chain(shell.args).collect(),
                crate::stdlib::process::execution_root_path(),
            )?;
            self.preparations.fetch_add(1, Ordering::SeqCst);
            Ok(HostCommandPreparation::Bound(Box::new(BoundProcess {
                calls: self.calls.clone(),
                invocation,
            })))
        })
    }

    fn dispatch<'a>(
        &'a self,
        _capability: &'a str,
        _operation: &'a str,
        _params: &'a crate::value::DictMap,
    ) -> HostCallDispatchFuture<'a> {
        Box::pin(async move {
            Err(VmError::Runtime(
                "process execution must consume its retained preparation".into(),
            ))
        })
    }
}
