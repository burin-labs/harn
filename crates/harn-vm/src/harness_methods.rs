//! Method dispatch for the `Harness` capability handle and its
//! sub-handles. Every sub-handle (`stdio`, `clock`, `fs`, `env`,
//! `random`, `net`, `process`, `crypto`, `system`, `secrets`, `llm`,
//! `tenant`, and `obs`)
//! is wired end-to-end in real, mock, and null modes;
//! sandbox / egress rejections raised inside a sub-handle method are
//! tagged with the `HARN-CAP-201` diagnostic code so callers can
//! attribute the error to the active capability profile rather than an
//! opaque tool rejection.
//!
//! These bodies reach layers above the VM, so the VM calls them only through
//! its `HarnessMethods` seam, which [`install`] fills.

use crate::value::VmDictExt;
use std::time::Duration;

use crate::harness::{vm_string, HarnessKind, HarnessMode, VmHarness};
use crate::harness_net::{
    self, record_audit, violation_request_value, violation_vm_error, NetPolicyAudit,
    NetPolicyDecision, NetPolicyMethodContract, OnViolation,
};
use crate::orchestration::RuntimeEffectState;
use crate::stdlib::io::{
    prompt_user_value, read_line_legacy_value, read_line_structured_value, write_stderr,
    write_stdout,
};
use crate::value::{ErrorCategory, VmError, VmValue};
use crate::vm::{HarnessMethodFuture, HarnessMethods, Vm};

mod verdict;

use verdict::call_harness_verdict_method;

/// The implementation of the VM's harness method seam.
struct StdHarnessMethods;

impl HarnessMethods for StdHarnessMethods {
    fn call<'a>(
        &self,
        vm: &'a mut Vm,
        handle: &'a VmHarness,
        method: &'a str,
        args: &'a [VmValue],
    ) -> HarnessMethodFuture<'a> {
        call_harness_method(vm, handle, method, args)
    }

    fn call_sync_fast(
        &self,
        output: &mut String,
        runtime_effects: &mut RuntimeEffectState,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        call_harness_method_sync_fast(output, runtime_effects, handle, method, args)
    }
}

static STD_HARNESS_METHODS: StdHarnessMethods = StdHarnessMethods;

/// Install these methods behind the VM's harness method seam.
pub(crate) fn install() {
    crate::vm::install_harness_methods(&STD_HARNESS_METHODS);
}

/// Outcome of `evaluate_net_policy_for_method`. `Allow` means the
/// dispatcher should proceed with the underlying call; `Deny` carries
/// the typed error to surface to the caller.
enum NetPolicyOutcome {
    Allow,
    Deny(VmError),
}

include!("harness_methods/dispatch.rs");
include!("harness_methods/capabilities.rs");
include!("harness_methods/native.rs");
include!("harness_methods/helpers.rs");
