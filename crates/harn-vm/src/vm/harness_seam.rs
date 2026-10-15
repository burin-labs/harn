//! The VM's seam to the `Harness` capability methods.
//!
//! The method bodies (the upper `harness_methods` module) reach secrets,
//! HTTP, the network policy, tenancy, orchestration and the stdlib, all of
//! which sit above the VM. The VM therefore calls them only through [`HarnessMethods`],
//! supplied by the VM's required runtime. Construction, forks and baselines
//! retain that owner; there is no process-global installation state.

use std::future::Future;
use std::pin::Pin;

use super::Vm;
use crate::runtime_effects::RuntimeEffectState;
use crate::value::VmHarness;
use crate::value::{VmError, VmValue};

/// The boxed future of one asynchronous harness method call. It is boxed so
/// a nested call keeps its capability-wide state machine off the executor
/// stack.
pub(crate) type HarnessMethodFuture<'a> =
    Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + Send + 'a>>;

/// Method dispatch for `Harness` capability handles, as the VM sees it.
pub(crate) trait HarnessMethods: Send + Sync {
    /// Run `handle.method(args)` with the receiver's clock scoped around it.
    fn call<'a>(
        &self,
        vm: &'a mut Vm,
        handle: &'a VmHarness,
        method: &'a str,
        args: &'a [VmValue],
    ) -> HarnessMethodFuture<'a>;

    /// Answer `handle.method(args)` synchronously when the method needs no
    /// VM re-entry. `None` sends the caller to [`HarnessMethods::call`].
    fn call_sync_fast(
        &self,
        output: &mut String,
        runtime_effects: &mut RuntimeEffectState,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>>;
}

impl Vm {
    pub(crate) async fn call_harness_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        let runtime = self.runtime.clone();
        runtime
            .harness_methods()
            .call(self, handle, method, args)
            .await
    }

    pub(in crate::vm) fn call_harness_method_sync_fast(
        methods: &dyn HarnessMethods,
        output: &mut String,
        runtime_effects: &mut RuntimeEffectState,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        methods.call_sync_fast(output, runtime_effects, handle, method, args)
    }
}
