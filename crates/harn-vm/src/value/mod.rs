mod core;
mod env;
mod error;
mod handles;
mod structural;

pub use core::{VmAsyncBuiltinFn, VmBuiltinFn, VmValue};
pub use env::{closest_match, ModuleFunctionRegistry, ModuleState, VmClosure, VmEnv};
pub use error::{
    categorized_error, classify_error_message, error_to_category, ErrorCategory, VmError,
};
pub use handles::{
    VmAtomicHandle, VmChannelHandle, VmGenerator, VmJoinHandle, VmRange, VmTaskHandle,
};
pub use structural::{
    compare_values, value_identity_key, value_structural_hash_key, values_equal, values_identical,
};

#[cfg(test)]
mod tests;
