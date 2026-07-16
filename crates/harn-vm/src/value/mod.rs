mod build;
mod core;
mod diff;
mod env;
mod error;
mod handles;
pub(crate) mod recursion;
mod set;
mod storage_json;
mod structural;

pub type VmMutex<T> = parking_lot::Mutex<T>;

pub use build::{DictRetain, VmDictExt};
pub use core::{
    intern_key, string_char_count, struct_fields_to_map, DictMap, HarnStr, StructInstanceData,
    StructLayout, VmAsyncBuiltinFn, VmBuiltinFn, VmBuiltinRefId, VmEnumVariant, VmValue,
};
pub use diff::{diff_values, render_diff, repr, DifferenceKind, ValueDifference};
pub(crate) use env::Binding;
pub use env::{
    closest_match, LazyVmCallable, ModuleFunctionRegistry, ModuleState, VmCallable, VmClosure,
    VmEnv,
};
pub use error::{
    categorized_error, classify_error_message, error_to_category, ArgTypeMismatchError,
    ArityExpect, ArityMismatchError, DeadlockError, ErrorCategory, VmError,
};
pub use handles::{
    VmAtomicHandle, VmChannelCloseState, VmChannelHandle, VmGenerator, VmJoinHandle, VmRange,
    VmRngHandle, VmStream, VmStreamCancel, VmSyncPermitHandle, VmTaskHandle,
};
pub use set::VmSet;
pub(crate) use storage_json::vm_to_storage_json;
pub use structural::{
    compare_values, dedup_values, try_compare_values, value_identity_key,
    value_structural_hash_key, values_equal, values_identical,
};

#[cfg(test)]
mod tests;
