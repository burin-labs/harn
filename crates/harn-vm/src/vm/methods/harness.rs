//! Method dispatch for the `Harness` capability handle and its
//! sub-handles. Every sub-handle (`stdio`, `clock`, `fs`, `env`,
//! `random`, `net`, `process`, `crypto`, `system`, `secrets`, `llm`,
//! `tenant`, and `obs`)
//! is wired end-to-end in real, mock, and null modes;
//! sandbox / egress rejections raised inside a sub-handle method are
//! tagged with the `HARN-CAP-201` diagnostic code so callers can
//! attribute the error to the active capability profile rather than an
//! opaque tool rejection.

use crate::value::VmDictExt;
use std::time::Duration;

use crate::harness::{vm_string, HarnessKind, HarnessMode, VmHarness};
use crate::harness_net::{
    self, record_audit, violation_request_value, violation_vm_error, NetPolicyAudit,
    NetPolicyDecision, NetPolicyMethodContract, OnViolation,
};
use crate::stdlib::io::{
    prompt_user_value, read_line_legacy_value, read_line_structured_value, write_stderr,
    write_stdout,
};
use crate::value::{ErrorCategory, VmError, VmValue};

/// Outcome of `Vm::evaluate_net_policy_for_method`. `Allow` means the
/// dispatcher should proceed with the underlying call; `Deny` carries
/// the typed error to surface to the caller.
enum NetPolicyOutcome {
    Allow,
    Deny(VmError),
}

include!("harness/dispatch.rs");
include!("harness/capabilities.rs");
include!("harness/native.rs");
include!("harness/helpers.rs");
