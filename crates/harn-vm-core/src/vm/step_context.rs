//! Typed call-context data shared by the interpreter and its execution runtime.

use crate::value::{VmError, VmValue};
use harn_execution_policy::StageDecl;
use std::sync::Arc;

/// Metadata registered when an annotated step declaration executes.
#[derive(Debug, Default, Clone)]
pub struct StepDefinition {
    pub name: String,
    pub function: String,
    pub model: Option<String>,
    pub max_tokens: Option<u64>,
    pub max_usd: Option<f64>,
    /// Budget exhaustion behavior: fail, continue, or escalate.
    pub error_boundary: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct PersonaDefinition {
    pub name: String,
    /// Stage order follows the authored manifest.
    pub stages: Vec<StageDecl>,
    pub output_style: Option<harn_modules::personas::PersonaOutputStyle>,
}

impl StepDefinition {
    pub fn boundary(&self) -> StepErrorBoundary {
        match self.error_boundary.as_deref() {
            Some("continue") => StepErrorBoundary::Continue,
            Some("escalate") => StepErrorBoundary::Escalate,
            _ => StepErrorBoundary::Fail,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepErrorBoundary {
    Fail,
    Continue,
    Escalate,
}

pub enum StepBoundaryDecision {
    Continue { message: String },
    Throw(VmError),
}

/// One tracked step, alive while its owning frame remains on the stack.
#[derive(Debug, Clone)]
pub struct ActiveStep {
    pub frame_depth: usize,
    pub definition: Arc<StepDefinition>,
    pub persona: Option<String>,
    pub args: Vec<VmValue>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub llm_calls: u32,
    pub last_model: Option<String>,
    /// Runtime tracing span, or zero when tracing was disabled at entry.
    pub span_id: u64,
    /// The runtime must pop a stage policy when this step leaves the stack.
    pub stage_policy_pushed: bool,
}

impl ActiveStep {
    pub fn new(
        frame_depth: usize,
        definition: Arc<StepDefinition>,
        persona: Option<String>,
        args: Vec<VmValue>,
        span_id: u64,
        stage_policy_pushed: bool,
    ) -> Self {
        Self {
            frame_depth,
            definition,
            persona,
            args,
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
            llm_calls: 0,
            last_model: None,
            span_id,
            stage_policy_pushed,
        }
    }

    pub fn total_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
}

#[derive(Debug, Clone)]
pub struct ActivePersona {
    pub frame_depth: usize,
    pub definition: Arc<PersonaDefinition>,
}

#[derive(Clone, Default)]
pub struct ActiveContextSnapshot {
    pub steps: Vec<ActiveStep>,
    pub personas: Vec<ActivePersona>,
}

/// Retain the exact caller context inline while the runtime suspends it.
/// The runtime's restorer decides whether this guard still owns the scope.
pub struct ActiveContextGuard {
    id: u64,
    outer: Option<ActiveContextSnapshot>,
    restore: fn(u64, ActiveContextSnapshot),
}

impl ActiveContextGuard {
    pub fn new(
        id: u64,
        outer: ActiveContextSnapshot,
        restore: fn(u64, ActiveContextSnapshot),
    ) -> Self {
        Self {
            id,
            outer: Some(outer),
            restore,
        }
    }
}

impl Drop for ActiveContextGuard {
    fn drop(&mut self) {
        (self.restore)(self.id, self.outer.take().expect("active context snapshot"));
    }
}
