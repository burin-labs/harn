//! Agent lifecycle transitions, worker lineage, and tool outcome contracts.

pub mod lifecycle;
pub mod terminal_suspension;
pub mod tool;
pub mod worker;

pub use lifecycle::{
    AgentLifecycle, AgentLifecycleEvent, AgentLifecycleProjection, AgentLifecycleState,
    LifecycleTransitionError,
};
pub use terminal_suspension::{AgentTerminalSuspension, ResumeWait};
pub use tool::{
    DenialGate, SideEffectCeilingDetails, SideEffectCeilingRemedy, StagedWriteSummary,
    ToolCallErrorCategory, ToolCallStatus, ToolDenial, ToolExecutor, ToolMutationStatus,
};
pub use worker::{
    AgentRunRef, DelegatedJoinBoundaries, DelegatedRunLineage, FsWatchEvent,
    SubagentTerminalStatus, WorkerEvent,
};
