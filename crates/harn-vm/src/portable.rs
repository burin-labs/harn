//! Native host adapter for versioned Portable Harn Kernel artifacts.
//!
//! This module deliberately contains no execution semantics. Native embedders
//! load the same bytes and enter the same transition machine as browser Wasm;
//! hostful Harn execution remains responsible for servicing typed capability
//! requests and feeding their results back through `resume`.

pub use harn_kernel::{
    CapabilityRequest, CapabilityResult, DataValue, Diagnostic, Execution, GrantSet,
};

use harn_kernel::{ArtifactLimits, ProgramArtifact};

/// Decode untrusted artifact bytes and start one isolated execution.
pub fn start(
    artifact: &[u8],
    input: DataValue,
    grants: &GrantSet,
) -> Result<Execution, Diagnostic> {
    let program = ProgramArtifact::decode(artifact, ArtifactLimits::default())?;
    Ok(harn_kernel::start(&program, input, grants))
}

/// Decode untrusted artifact bytes and resume one authenticated snapshot.
pub fn resume(
    artifact: &[u8],
    snapshot: &[u8],
    result: CapabilityResult,
    grants: &GrantSet,
) -> Result<Execution, Diagnostic> {
    let program = ProgramArtifact::decode(artifact, ArtifactLimits::default())?;
    Ok(harn_kernel::resume(&program, snapshot, result, grants))
}
