//! Required runtime state for an interpreter and its derived instances.

use std::{any::Any, future::Future, path::Path, pin::Pin, sync::Arc};

use crate::value::{VmError, VmValue};

pub(crate) enum BuiltinGateDecision {
    Continue,
    Return(VmValue),
}

pub(crate) trait VmBuiltinBridge: Send + Sync {
    fn call_builtin<'a>(
        &'a self,
        name: &'a str,
        args: &'a [VmValue],
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + Send + 'a>>;
}

pub(crate) trait VmRuntime: Any + Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn fork_child(&self) -> Arc<dyn VmRuntime>;
    fn baseline(&self) -> Arc<dyn VmRuntimeBaseline>;
    fn builtin_bridge(&self) -> Option<&dyn VmBuiltinBridge>;
    fn harness_methods(&self) -> &dyn super::HarnessMethods;
    fn top_level_scope(&self, vm: &mut super::Vm) -> Box<dyn super::VmExecutionScope>;
    fn rebind_execution_state(&self, vm: &mut super::Vm);
    fn set_source_dir(&self, dir: &Path);
    fn denied_builtin_family(
        &self,
        denied: std::collections::HashSet<String>,
    ) -> std::collections::HashSet<String>;
    fn requires_async_builtin_gate(&self, name: &str) -> bool;
    fn gate_preparation(
        &self,
        name: &str,
        contract: Option<&harn_builtin_meta::BuiltinContract>,
    ) -> Result<(), VmError>;
    fn gate_sync_builtin(&self, name: &str, args: &[VmValue]) -> Result<(), VmError>;
    fn gate_async_builtin<'a>(
        &'a self,
        name: &'a str,
        args: &'a [VmValue],
    ) -> Pin<Box<dyn Future<Output = Result<BuiltinGateDecision, VmError>> + Send + 'a>>;
    fn gate_bridge_builtin(&self, name: &str) -> Result<(), VmError>;
    fn builtin_effects(&self, name: &str) -> Option<&'static [harn_builtin_meta::EffectSpec]>;
    fn capability_effects(
        &self,
        capability: harn_builtin_meta::CapabilityId,
        method: &str,
    ) -> Option<&'static [harn_builtin_meta::EffectSpec]>;
}

pub(crate) trait VmRuntimeBaseline: Send + Sync {
    fn instantiate(&self, source_dir: Option<&Path>) -> Arc<dyn VmRuntime>;
}
