use super::AgentEvent;

impl AgentEvent {
    pub fn feedback_injected(
        session_id: impl Into<String>,
        kind: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self::FeedbackInjected {
            session_id: session_id.into(),
            kind: kind.into(),
            content: content.into(),
            streak: None,
            iteration: None,
            tool_name: None,
            turn_claimed_for_repair: None,
            delivered: None,
        }
    }

    pub fn session_id(&self) -> &str {
        match self {
            Self::TurnPhaseChanged { session_id, .. }
            | Self::SessionHealth { session_id, .. }
            | Self::AgentMessageChunk { session_id, .. }
            | Self::AgentThoughtChunk { session_id, .. }
            | Self::UserMessage { session_id, .. }
            | Self::ToolCall { session_id, .. }
            | Self::ToolCallUpdate { session_id, .. }
            | Self::PlanDocumentUpdated { session_id, .. }
            | Self::OrchestrationDecision { session_id, .. }
            | Self::ProgressReported { session_id, .. }
            | Self::PurposeLabel { session_id, .. }
            | Self::CompassRoutingDecision { session_id, .. }
            | Self::AgentScratchpadReorganization { session_id, .. }
            | Self::Artifact { session_id, .. }
            | Self::IterationStart { session_id, .. }
            | Self::IterationEnd { session_id, .. }
            | Self::SessionClosed { session_id, .. }
            | Self::AnchorChanged { session_id, .. }
            | Self::JudgeStarted { session_id, .. }
            | Self::JudgeDecision { session_id, .. }
            | Self::StepJudgeDecision { session_id, .. }
            | Self::StructuralValidatorDecision { session_id, .. }
            | Self::ScopeClassifierVerdict { session_id, .. }
            | Self::InputGuardrailVerdict { session_id, .. }
            | Self::MissingToolCallVerdict { session_id, .. }
            | Self::RepairOutputContractApplied { session_id, .. }
            | Self::RequireSuccessfulToolsViolation { session_id, .. }
            | Self::FinalWrapup { session_id, .. }
            | Self::PackThinkingStripped { session_id, .. }
            | Self::SelfConsistencyTie { session_id, .. }
            | Self::CodeLibrarianQueryNlFallback { session_id, .. }
            | Self::TypedCheckpoint { session_id, .. }
            | Self::ModelJob { session_id, .. }
            | Self::FeedbackInjected { session_id, .. }
            | Self::HostToolResult { session_id, .. }
            | Self::HostAttachment { session_id, .. }
            | Self::BudgetExhausted { session_id, .. }
            | Self::BudgetCircuitBreaker { session_id, .. }
            | Self::LoopStuck { session_id, .. }
            | Self::LoopStuckSignal { session_id, .. }
            | Self::ReservedTerminalVerify { session_id, .. }
            | Self::DaemonWatchdogTripped { session_id, .. }
            | Self::SkillActivated { session_id, .. }
            | Self::SkillDeactivated { session_id, .. }
            | Self::SkillScopeTools { session_id, .. }
            | Self::SkillNarrow { session_id, .. }
            | Self::StanceTransition { session_id, .. }
            | Self::ToolSearchQuery { session_id, .. }
            | Self::ToolSearchResult { session_id, .. }
            | Self::TranscriptCompacted { session_id, .. }
            | Self::TranscriptProjected { session_id, .. }
            | Self::ReminderEmitted { session_id, .. }
            | Self::Handoff { session_id, .. }
            | Self::FsWatch { session_id, .. }
            | Self::StagedWritesPending { session_id, .. }
            | Self::SafeTextPatchResult { session_id, .. }
            | Self::ControlOutcome { session_id, .. }
            | Self::WorkerUpdate { session_id, .. }
            | Self::SubagentStop { session_id, .. }
            | Self::SubagentJoin { session_id, .. }
            | Self::HitlRequested { session_id, .. }
            | Self::HitlResolved { session_id, .. }
            | Self::LoopControlDecision { session_id, .. }
            | Self::AgentLoopStallWarning { session_id, .. }
            | Self::CapabilityGap { session_id, .. }
            | Self::ToolFormatOverride { session_id, .. }
            | Self::ToolCallAudit { session_id, .. }
            | Self::ToolBatchDisposition { session_id, .. }
            | Self::CacheHit { session_id, .. }
            | Self::CacheMiss { session_id, .. }
            | Self::LlmCallLog { session_id, .. }
            | Self::LlmRoutingDecision { session_id, .. }
            | Self::LlmFallbackAttempt { session_id, .. }
            | Self::LlmShadowDiff { session_id, .. }
            | Self::SemanticCacheHit { session_id, .. }
            | Self::SemanticCacheMiss { session_id, .. }
            | Self::CompositionStart { session_id, .. }
            | Self::CompositionChildCall { session_id, .. }
            | Self::CompositionChildResult { session_id, .. }
            | Self::CompositionFinish { session_id, .. }
            | Self::CompositionError { session_id, .. }
            | Self::LoopCheckpoint { session_id, .. }
            | Self::McpNotification { session_id, .. }
            | Self::McpCatalogChanged { session_id, .. }
            | Self::McpAuthRequired { session_id, .. }
            | Self::BoundaryFailure { session_id, .. } => session_id,
        }
    }
}
