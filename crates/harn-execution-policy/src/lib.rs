//! Pure execution authority and tool semantics shared by native runtimes.

mod autonomy;
mod capability;
mod capability_lattice;
pub mod known_paths;
pub mod session_inputs;
mod stage;
pub mod tool_annotations;

pub use autonomy::AutonomyTier;
pub use capability::{
    default_read_deny_home_paths, CapabilityPolicy, LoopbackEnforcement, ProcessNetworkProxy,
    ProcessSandboxPolicy, ProcessSandboxPreset, SandboxProfile, ToolArgConstraint,
    UnixSocketEnforcement,
};
pub use capability_lattice::{operation_is_covered, policy_allows_capability};
pub use stage::{StageDecl, StageExit};
