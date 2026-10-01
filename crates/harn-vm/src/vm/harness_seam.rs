//! The VM's seam to the `Harness` capability methods.
//!
//! The method bodies (the upper `harness_methods` module) reach secrets,
//! HTTP, the network policy, tenancy, orchestration and the stdlib, all of
//! which sit above the VM. The VM therefore calls them only through [`HarnessMethods`],
//! which the upper layer installs once per process from
//! [`crate::initialize_runtime_assets`]. Every VM constructor (`Vm::new` and
//! `VmBaseline::instantiate`) calls that initializer, and every child VM is
//! derived from one of them, so an installed process never observes the
//! uninstalled state.
//!
//! The seam fails closed: until an implementation is installed, every harness
//! method call returns an error rather than succeeding or doing nothing.

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;

use super::Vm;
use crate::harness::VmHarness;
use crate::orchestration::RuntimeEffectState;
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

static HARNESS_METHODS: OnceLock<&'static dyn HarnessMethods> = OnceLock::new();

/// Install the process's harness method implementation. The first install
/// wins; later calls are no-ops.
pub(crate) fn install_harness_methods(methods: &'static dyn HarnessMethods) {
    HARNESS_METHODS.get_or_init(|| methods);
}

fn installed(
    slot: &OnceLock<&'static dyn HarnessMethods>,
) -> Result<&'static dyn HarnessMethods, VmError> {
    slot.get().copied().ok_or_else(|| {
        VmError::Runtime("harness capability methods are not installed in this runtime".to_string())
    })
}

impl Vm {
    pub(crate) async fn call_harness_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        installed(&HARNESS_METHODS)?
            .call(self, handle, method, args)
            .await
    }

    pub(in crate::vm) fn call_harness_method_sync_fast(
        output: &mut String,
        runtime_effects: &mut RuntimeEffectState,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        match installed(&HARNESS_METHODS) {
            Ok(methods) => methods.call_sync_fast(output, runtime_effects, handle, method, args),
            Err(error) => Some(Err(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Unreachable;

    impl HarnessMethods for Unreachable {
        fn call<'a>(
            &self,
            _vm: &'a mut Vm,
            _handle: &'a VmHarness,
            _method: &'a str,
            _args: &'a [VmValue],
        ) -> HarnessMethodFuture<'a> {
            unreachable!("the seam test never dispatches")
        }

        fn call_sync_fast(
            &self,
            _output: &mut String,
            _runtime_effects: &mut RuntimeEffectState,
            _handle: &VmHarness,
            _method: &str,
            _args: &[VmValue],
        ) -> Option<Result<VmValue, VmError>> {
            unreachable!("the seam test never dispatches")
        }
    }

    static UNREACHABLE: Unreachable = Unreachable;

    #[test]
    fn an_uninstalled_seam_is_an_error() {
        let slot = OnceLock::new();
        let Err(VmError::Runtime(message)) = installed(&slot) else {
            panic!("an empty seam must not resolve to an implementation");
        };
        assert_eq!(
            message,
            "harness capability methods are not installed in this runtime"
        );
        slot.get_or_init(|| &UNREACHABLE as &'static dyn HarnessMethods);
        assert!(installed(&slot).is_ok());
    }

    #[test]
    fn vm_new_installs_the_seam() {
        let _vm = Vm::new();
        assert!(installed(&HARNESS_METHODS).is_ok());
    }
}
