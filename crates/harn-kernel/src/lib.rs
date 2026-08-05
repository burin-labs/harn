//! Portable Harn compiler and deterministic execution kernel.
//!
//! This crate is deliberately a dependency leaf with no filesystem, network,
//! process, clock, random, model, or async-runtime authority.

/// Version of the crate that owns portable artifact and execution semantics.
/// Adapters use this value directly so benchmark provenance cannot drift to
/// the version of whichever host happens to emit a receipt.
pub const KERNEL_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Shared adapter ingress limits. Native and browser hosts enforce these
/// before allocating or parsing untrusted wire inputs.
pub const PORTABLE_MAX_SOURCE_BYTES: usize = 1024 * 1024;
pub const PORTABLE_MAX_PACKAGE_BYTES: usize = 8 * 1024 * 1024;
pub const PORTABLE_MAX_PACKAGE_MODULES: usize = 1_024;
pub const PORTABLE_MAX_VALUE_JSON_BYTES: usize = 1024 * 1024;
pub const PORTABLE_MAX_GRANTS_JSON_BYTES: usize = 64 * 1024;

pub mod artifact;
pub mod benchmark;
mod builtin_id;
pub mod compiler;
pub mod opcode;
mod portable_builtin;
pub mod program;
pub mod pure;
mod runtime_limits;
mod schema;
pub mod type_contract;
pub mod value;

pub use artifact::{
    compile_program, compile_program_package, compile_source_package, semantic_abi_fingerprint_hex,
    ArtifactLimits, Diagnostic, EntryKind, PortableModuleSource, PortablePackageSource,
    ProgramArtifact, ProgramModule, ARTIFACT_VERSION,
};
pub use benchmark::{
    benchmark_terminal_digest, portable_benchmark_json_schema, BenchmarkBuildProfile,
    BenchmarkEntryKind, BenchmarkProvenance, BenchmarkStatistics, BenchmarkStatisticsError,
    BenchmarkTarget, CompileMeasurements, DispatchMeasurements, PortableBenchmarkReceipt,
    PORTABLE_BENCHMARK_SCHEMA_VERSION, PORTABLE_MAX_COMPILE_ITERATIONS,
    PORTABLE_MAX_DISPATCH_ITERATIONS, PORTABLE_MAX_WORKERS,
};
pub use builtin_id::BuiltinId;
pub use compiler::{
    CompileError, CompiledCallableEntry, CompiledPortableModule, Compiler, CompilerOptions,
    PortableExportKind, PortableImport, PortableSourceModule, PortableSourcePackage,
};
pub use execution::{
    replay, resume, start, CapabilityRequest, CapabilityResult, DataValue, Execution, GrantSet,
    ValueShape, PORTABLE_MAX_SNAPSHOT_BYTES,
};
pub use opcode::{
    opcode_abi_fingerprint, Op, OperandKind, Portability, OPCODE_ABI_ARTIFACT_VERSION,
    OPCODE_ABI_FINGERPRINT_V3,
};
pub use program::{BindingTypeSlot, Chunk, CompiledFunction, Constant, LocalSlotInfo, ParamSlot};

mod chunk {
    pub use crate::program::*;
    pub use crate::Op;
}

/// Compile a checked source module with deterministic options.
pub fn compile_source(source: &str) -> Result<Chunk, String> {
    let program = harn_parser::check_source_strict(source).map_err(|error| error.to_string())?;
    Compiler::new()
        .compile(&program)
        .map_err(|error| error.to_string())
}

pub fn compile_source_named(source: &str, pipeline_name: &str) -> Result<Chunk, String> {
    let program = harn_parser::check_source_strict(source).map_err(|error| error.to_string())?;
    let exists = program.iter().any(|node| {
        let (_, inner) = harn_parser::peel_attributes(node);
        matches!(&inner.node, harn_parser::Node::Pipeline { name, .. } if name == pipeline_name)
    });
    if !exists {
        return Err(format!("no pipeline named `{pipeline_name}` in source"));
    }
    Compiler::new()
        .compile_named(&program, pipeline_name)
        .map_err(|error| error.to_string())
}
pub mod execution;
