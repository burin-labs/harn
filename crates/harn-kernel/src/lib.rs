//! Portable Harn compiler and deterministic execution kernel.
//!
//! This crate is deliberately a dependency leaf with no filesystem, network,
//! process, clock, random, model, or async-runtime authority.

/// Version of the crate that owns portable artifact and execution semantics.
/// Adapters use this value directly so benchmark provenance cannot drift to
/// the version of whichever host happens to emit a receipt.
pub const KERNEL_VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod artifact;
pub mod benchmark;
mod builtin_id;
pub mod compiler;
pub mod opcode;
mod portable_builtin;
pub mod program;
mod runtime_limits;
mod schema;
pub mod type_contract;
pub mod value;

pub use artifact::{
    compile_program, semantic_abi_fingerprint_hex, ArtifactLimits, Diagnostic, EntryKind,
    ProgramArtifact, ARTIFACT_VERSION,
};
pub use benchmark::{
    benchmark_terminal_digest, portable_benchmark_json_schema, BenchmarkBuildProfile,
    BenchmarkEntryKind, BenchmarkProvenance, BenchmarkStatistics, BenchmarkStatisticsError,
    BenchmarkTarget, CompileMeasurements, DispatchMeasurements, PortableBenchmarkReceipt,
    PORTABLE_BENCHMARK_SCHEMA_VERSION, PORTABLE_MAX_COMPILE_ITERATIONS,
    PORTABLE_MAX_DISPATCH_ITERATIONS, PORTABLE_MAX_WORKERS,
};
pub use builtin_id::BuiltinId;
pub use compiler::{CompileError, CompiledCallableEntry, Compiler, CompilerOptions};
pub use execution::{
    replay, resume, start, CapabilityRequest, CapabilityResult, DataValue, Execution, GrantSet,
    ValueShape,
};
pub use opcode::{
    opcode_abi_fingerprint, Op, OperandKind, Portability, OPCODE_ABI_ARTIFACT_VERSION,
    OPCODE_ABI_FINGERPRINT_V1,
};
pub use program::{Chunk, CompiledFunction, Constant, LocalSlotInfo, ParamSlot};

/// Compatibility namespace kept private to the compiler implementation while
/// the public contract calls this immutable structure a program image.
pub mod chunk {
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
