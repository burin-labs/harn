pub mod args;
mod build;
mod core;
mod diff;
mod env;
mod error;
mod error_convert;
mod handles;
mod harness;
mod host_object;
mod io_error;
mod isolate;
pub mod json;
mod mcp_client;
mod random;
pub mod recursion;
mod sandbox_refusal;
mod semantics;
mod set;
mod storage_json;
mod structural;

pub type VmMutex<T> = parking_lot::Mutex<T>;

pub use build::{DictRetain, VmDictExt};
pub use core::{
    byte_offset_to_char_index, char_range_to_byte_range, char_to_byte_offset, intern_key,
    string_char_count, struct_fields_to_map, DictMap, HarnStr, StructInstanceData, StructLayout,
    VmAsyncBuiltinFn, VmBuiltinFn, VmBuiltinRefId, VmEnumVariant, VmValue,
};
pub use diff::{diff_values, render_diff, repr, DifferenceKind, ValueDifference};
pub(crate) use env::Binding;
pub use env::{
    closest_match, LazyPipelineCallable, LazyVmCallable, ModuleFunctionRegistry, ModuleState,
    VmCallable, VmClosure, VmEnv,
};
pub use error::error_category_for_http_status;
pub use error::{
    categorized_error, classify_error_message, error_to_category, ArgTypeMismatchError,
    ArityExpect, ArityMismatchError, BindingTypeMismatchError, DeadlockError, ErrorCategory,
    McpInputRequired, ProviderStreamDeadline, ProviderStreamFailure, ProviderStreamFailureReason,
    ProviderStreamPhase, SchemaStreamAbort, SchemaValidationReasonKind, ValueByteLimitError,
    VmError,
};
pub use handles::{
    VmAtomicHandle, VmChannelCloseState, VmChannelHandle, VmGenerator, VmJoinHandle, VmRange,
    VmResourceGuardHandle, VmResourceHandle, VmRngHandle, VmStream, VmStreamCancel,
    VmSyncPermitHandle, VmTaskHandle, VmVerdictReceipt,
};
pub use harness::VmHarness;
pub use host_object::VmHostObject;
pub use io_error::{
    environment_io_error_thrown, io_error_kind_str, io_error_thrown, io_error_value,
};
pub use isolate::IsolateValue;
pub use mcp_client::VmMcpClientHandle;
pub use random::RandomOperation;
pub use sandbox_refusal::NO_OS_SANDBOX;
pub use sandbox_refusal::{
    ConfinementDimension, HostProtectionGap, SandboxMechanism, SandboxMechanismAvailability,
    SandboxMechanismUnavailable, SandboxRequirement,
};
pub use semantics::{char_substring, split_separator_required, string_discriminator};
pub use set::VmSet;
pub use storage_json::vm_to_storage_json;
pub use structural::{
    compare_values, dedup_values, try_compare_values, value_equality_hash_key, value_identity_key,
    value_structural_hash_key, values_equal, values_identical,
};

#[cfg(test)]
mod tests;
