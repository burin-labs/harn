mod build;
mod core;
mod env;
mod error;
mod handles;
pub(crate) mod recursion;
mod structural;

pub type VmMutex<T> = parking_lot::Mutex<T>;

pub use build::VmDictExt;
pub use core::{
    string_char_count, struct_fields_to_map, StructLayout, VmAsyncBuiltinFn, VmBuiltinFn,
    VmBuiltinRefId, VmEnumVariant, VmValue,
};
pub use env::{closest_match, ModuleFunctionRegistry, ModuleState, VmClosure, VmEnv};
pub use error::{
    categorized_error, classify_error_message, error_to_category, ArgTypeMismatchError,
    ArityExpect, ArityMismatchError, DeadlockError, ErrorCategory, VmError,
};
pub use handles::{
    VmAtomicHandle, VmChannelCloseState, VmChannelHandle, VmGenerator, VmJoinHandle, VmRange,
    VmRngHandle, VmStream, VmStreamCancel, VmSyncPermitHandle, VmTaskHandle,
};
pub use structural::{
    compare_values, dedup_values, try_compare_values, value_identity_key,
    value_structural_hash_key, values_equal, values_identical,
};

#[cfg(test)]
mod tests;
