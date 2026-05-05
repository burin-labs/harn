//! Per-step runtime state for `@step`-annotated persona functions.
//!
//! The compiler emits a call to the `__register_step` builtin after each
//! `@step` declaration so the runtime can dispatch on the step's metadata
//! when its function is invoked. While a step's frame is on the call
//! stack, an [`ActiveStep`] entry tracks per-step LLM usage, defaults
//! `llm_call`'s model when the call site doesn't override it, and bounds
//! cumulative token and cost spend against the step's budget.
//!
//! This module owns three thread-locals (a per-program registry, a stack
//! of currently-active steps, and a log of completed step summaries) but
//! exposes only narrow helpers — `current_active_step_*` /
//! `record_step_llm_usage` / etc. — so the call sites in
//! `crates/harn-vm/src/llm/`, `crates/harn-vm/src/vm/`, and the compiler
//! stay focused.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::orchestration::HookEvent;
use crate::value::{VmClosure, VmError, VmValue};

fn vm_str(value: &VmValue) -> Option<&str> {
    match value {
        VmValue::String(s) => Some(s.as_ref()),
        _ => None,
    }
}

/// Static metadata captured from a `@step(...)` attribute.
///
/// Populated by the `__register_step` builtin (see [`register_step_from_dict`])
/// when the program first runs, then consulted by `llm_call` and the
/// frame-pop hooks while the step is active.
#[derive(Debug, Default, Clone)]
pub struct StepDefinition {
    pub name: String,
    pub function: String,
    pub model: Option<String>,
    pub max_tokens: Option<u64>,
    pub max_usd: Option<f64>,
    /// One of "fail" (default), "continue", "escalate". Drives how a
    /// `budget_exceeded` error propagating out of the step is handled —
    /// see `crates/harn-vm/src/vm/execution.rs`.
    pub error_boundary: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct PersonaDefinition {
    pub name: String,
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

/// Tracks one in-flight step. The `frame_depth` is `Vm::frames.len()`
/// captured immediately after `push_closure_frame` returns, so an
/// `ActiveStep` is "alive" while `Vm::frames.len() >= frame_depth`.
#[derive(Debug, Clone)]
pub struct ActiveStep {
    pub frame_depth: usize,
    pub definition: Rc<StepDefinition>,
    pub persona: Option<String>,
    pub args: Vec<VmValue>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub llm_calls: u32,
    pub last_model: Option<String>,
}

impl ActiveStep {
    fn new(
        frame_depth: usize,
        definition: Rc<StepDefinition>,
        persona: Option<String>,
        args: Vec<VmValue>,
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
        }
    }

    fn total_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
}

#[derive(Debug, Clone)]
pub struct ActivePersona {
    pub frame_depth: usize,
    pub definition: Rc<PersonaDefinition>,
}

/// Snapshot persisted into [`COMPLETED_STEPS`] when the step's frame
/// unwinds. Receipts and `harn persona inspect`-style downstream consumers
/// read it back via [`drain_completed_steps`].
#[derive(Debug, Clone, Serialize)]
pub struct CompletedStep {
    pub name: String,
    pub function: String,
    pub model: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub llm_calls: u32,
    pub status: String,
    pub error: Option<String>,
}

thread_local! {
    static STEP_REGISTRY: RefCell<BTreeMap<String, Rc<StepDefinition>>> =
        const { RefCell::new(BTreeMap::new()) };
    static PERSONA_REGISTRY: RefCell<BTreeMap<String, Rc<PersonaDefinition>>> =
        const { RefCell::new(BTreeMap::new()) };
    static PERSONA_STACK: RefCell<Vec<ActivePersona>> = const { RefCell::new(Vec::new()) };
    static STEP_STACK: RefCell<Vec<ActiveStep>> = const { RefCell::new(Vec::new()) };
    static COMPLETED_STEPS: RefCell<Vec<CompletedStep>> = const { RefCell::new(Vec::new()) };
    static PERSONA_HOOKS: RefCell<Vec<PersonaHookRegistration>> = const { RefCell::new(Vec::new()) };
}

/// Reset every thread-local owned by this module. Called between test
/// runs and at the start of each top-level program execution so leftover
/// registrations don't leak across runs.
pub fn reset_thread_local_state() {
    STEP_REGISTRY.with(|r| r.borrow_mut().clear());
    PERSONA_REGISTRY.with(|r| r.borrow_mut().clear());
    PERSONA_STACK.with(|s| s.borrow_mut().clear());
    STEP_STACK.with(|s| s.borrow_mut().clear());
    COMPLETED_STEPS.with(|c| c.borrow_mut().clear());
    PERSONA_HOOKS.with(|h| h.borrow_mut().clear());
}

/// Bind a `@step` function name to its declared metadata. Idempotent: a
/// second call replaces the prior definition (matches re-evaluation
/// semantics of `harn run` and the conformance harness).
pub fn register_step(function: &str, definition: StepDefinition) {
    STEP_REGISTRY.with(|registry| {
        registry
            .borrow_mut()
            .insert(function.to_string(), Rc::new(definition));
    });
}

pub fn register_persona(function: &str, definition: PersonaDefinition) {
    PERSONA_REGISTRY.with(|registry| {
        registry
            .borrow_mut()
            .insert(function.to_string(), Rc::new(definition));
    });
}

pub fn register_persona_from_dict(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let function = args
        .first()
        .and_then(vm_str)
        .map(|s| s.to_string())
        .ok_or_else(|| {
            VmError::Thrown(VmValue::String(Rc::from(
                "__register_persona: expected (function_name, metadata_dict)",
            )))
        })?;
    let meta = args
        .get(1)
        .and_then(VmValue::as_dict)
        .cloned()
        .ok_or_else(|| {
            VmError::Thrown(VmValue::String(Rc::from(
                "__register_persona: metadata argument must be a dict",
            )))
        })?;
    let definition = PersonaDefinition {
        name: meta
            .get("name")
            .and_then(vm_str)
            .map(str::to_string)
            .unwrap_or_else(|| function.clone()),
    };
    register_persona(&function, definition);
    Ok(VmValue::Nil)
}

/// Builtin entry point invoked by compiler-emitted bytecode after every
/// `@step` function declaration. Accepts a dict mirroring
/// `harn_modules::PersonaStepMetadata`.
pub fn register_step_from_dict(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let function = args
        .first()
        .and_then(vm_str)
        .map(|s| s.to_string())
        .ok_or_else(|| {
            VmError::Thrown(VmValue::String(Rc::from(
                "__register_step: expected (function_name, metadata_dict)",
            )))
        })?;
    let meta = args
        .get(1)
        .and_then(VmValue::as_dict)
        .cloned()
        .ok_or_else(|| {
            VmError::Thrown(VmValue::String(Rc::from(
                "__register_step: metadata argument must be a dict",
            )))
        })?;

    let mut definition = StepDefinition {
        function: function.clone(),
        ..StepDefinition::default()
    };
    definition.name = meta
        .get("name")
        .and_then(vm_str)
        .map(|s| s.to_string())
        .unwrap_or_else(|| function.clone());
    definition.model = meta
        .get("model")
        .and_then(vm_str)
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());
    definition.error_boundary = meta
        .get("error_boundary")
        .and_then(vm_str)
        .map(|s| s.to_string());

    if let Some(VmValue::Dict(budget)) = meta.get("budget") {
        if let Some(value) = budget.get("max_tokens") {
            definition.max_tokens = match value {
                VmValue::Int(n) if *n > 0 => Some(*n as u64),
                VmValue::Float(f) if f.is_finite() && *f > 0.0 => Some(*f as u64),
                _ => None,
            };
        }
        if let Some(value) = budget.get("max_usd") {
            definition.max_usd = match value {
                VmValue::Float(f) if f.is_finite() && *f >= 0.0 => Some(*f),
                VmValue::Int(n) if *n >= 0 => Some(*n as f64),
                _ => None,
            };
        }
    }

    register_step(&function, definition);
    Ok(VmValue::Nil)
}

#[derive(Clone)]
pub struct PersonaHookRegistration {
    pub persona_pattern: String,
    pub step_name: Option<String>,
    pub event: HookEvent,
    pub threshold_pct: Option<f64>,
    pub handler: Rc<VmClosure>,
}

impl std::fmt::Debug for PersonaHookRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersonaHookRegistration")
            .field("persona_pattern", &self.persona_pattern)
            .field("step_name", &self.step_name)
            .field("event", &self.event)
            .field("threshold_pct", &self.threshold_pct)
            .field("handler", &"..")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct PersonaHookInvocation {
    pub handler: Rc<VmClosure>,
    pub event: HookEvent,
}

pub fn register_persona_hook(
    persona_pattern: impl Into<String>,
    event: HookEvent,
    threshold_pct: Option<f64>,
    handler: Rc<VmClosure>,
) {
    PERSONA_HOOKS.with(|hooks| {
        hooks.borrow_mut().push(PersonaHookRegistration {
            persona_pattern: persona_pattern.into(),
            step_name: None,
            event,
            threshold_pct,
            handler,
        });
    });
}

pub fn register_step_hook(
    persona_pattern: impl Into<String>,
    step_name: impl Into<String>,
    event: HookEvent,
    threshold_pct: Option<f64>,
    handler: Rc<VmClosure>,
) {
    PERSONA_HOOKS.with(|hooks| {
        hooks.borrow_mut().push(PersonaHookRegistration {
            persona_pattern: persona_pattern.into(),
            step_name: Some(step_name.into()),
            event,
            threshold_pct,
            handler,
        });
    });
}

pub fn clear_persona_hooks() {
    PERSONA_HOOKS.with(|hooks| hooks.borrow_mut().clear());
}

pub struct ActiveContextSnapshot {
    steps: Vec<ActiveStep>,
    personas: Vec<ActivePersona>,
}

pub fn take_active_context() -> ActiveContextSnapshot {
    ActiveContextSnapshot {
        steps: STEP_STACK.with(|stack| std::mem::take(&mut *stack.borrow_mut())),
        personas: PERSONA_STACK.with(|stack| std::mem::take(&mut *stack.borrow_mut())),
    }
}

pub fn restore_active_context(snapshot: ActiveContextSnapshot) {
    STEP_STACK.with(|stack| *stack.borrow_mut() = snapshot.steps);
    PERSONA_STACK.with(|stack| *stack.borrow_mut() = snapshot.personas);
}

pub fn is_tracked_function(function_name: &str) -> bool {
    STEP_REGISTRY.with(|registry| registry.borrow().contains_key(function_name))
        || PERSONA_REGISTRY.with(|registry| registry.borrow().contains_key(function_name))
}

pub fn step_definition_for_function(function_name: &str) -> Option<Rc<StepDefinition>> {
    STEP_REGISTRY.with(|registry| registry.borrow().get(function_name).cloned())
}

pub fn current_persona_name() -> Option<String> {
    PERSONA_STACK.with(|stack| stack.borrow().last().map(|p| p.definition.name.clone()))
}

fn persona_matches(pattern: &str, persona: &str) -> bool {
    crate::orchestration::glob_match(pattern, persona)
}

pub fn matching_hooks(
    event: HookEvent,
    persona: Option<&str>,
    step_name: Option<&str>,
    budget_pct: Option<f64>,
) -> Vec<PersonaHookInvocation> {
    let persona = persona.unwrap_or("");
    PERSONA_HOOKS.with(|hooks| {
        hooks
            .borrow()
            .iter()
            .filter(|hook| hook.event == event)
            .filter(|hook| persona_matches(&hook.persona_pattern, persona))
            .filter(|hook| match (&hook.step_name, step_name) {
                (Some(expected), Some(actual)) => expected == actual,
                (Some(_), None) => false,
                (None, _) => true,
            })
            .filter(|hook| match (hook.threshold_pct, budget_pct) {
                (Some(threshold), Some(pct)) => pct >= threshold,
                (Some(_), None) => false,
                (None, _) => true,
            })
            .map(|hook| PersonaHookInvocation {
                handler: hook.handler.clone(),
                event: hook.event,
            })
            .collect()
    })
}

pub fn maybe_push_active_persona(function_name: &str, frame_depth: usize) -> bool {
    let definition =
        PERSONA_REGISTRY.with(|registry| registry.borrow().get(function_name).cloned());
    let Some(definition) = definition else {
        return false;
    };
    PERSONA_STACK.with(|stack| {
        stack.borrow_mut().push(ActivePersona {
            frame_depth,
            definition,
        });
    });
    true
}

/// Push an active step onto the stack iff `function_name` has metadata
/// registered. Returns `true` when a frame was pushed so the call site
/// can record that fact. Called from `Vm::push_closure_frame` after the
/// new frame has been added.
pub fn maybe_push_active_step(function_name: &str, frame_depth: usize, args: &[VmValue]) -> bool {
    let definition = STEP_REGISTRY.with(|registry| registry.borrow().get(function_name).cloned());
    let Some(definition) = definition else {
        return false;
    };
    let persona = current_persona_name();
    STEP_STACK.with(|stack| {
        stack.borrow_mut().push(ActiveStep::new(
            frame_depth,
            definition,
            persona,
            args.to_vec(),
        ));
    });
    true
}

/// Drop any step entries whose owning frame has already been unwound,
/// recording a `CompletedStep` summary for each. The `current_frame_depth`
/// is `Vm::frames.len()` at the call site — entries with
/// `frame_depth > current_frame_depth` are stale.
pub fn prune_below_frame(current_frame_depth: usize) {
    let mut popped: Vec<ActiveStep> = Vec::new();
    STEP_STACK.with(|stack| {
        let mut stack = stack.borrow_mut();
        while let Some(top) = stack.last() {
            if top.frame_depth > current_frame_depth {
                popped.push(stack.pop().unwrap());
            } else {
                break;
            }
        }
    });
    for step in popped {
        finish_step(step, "completed", None);
    }
    PERSONA_STACK.with(|stack| {
        let mut stack = stack.borrow_mut();
        while stack
            .last()
            .is_some_and(|persona| persona.frame_depth > current_frame_depth)
        {
            stack.pop();
        }
    });
}

pub fn take_active_step(current_frame_depth: usize) -> Option<ActiveStep> {
    STEP_STACK.with(|stack| {
        let mut stack = stack.borrow_mut();
        if stack
            .last()
            .is_some_and(|step| step.frame_depth == current_frame_depth)
        {
            stack.pop()
        } else {
            None
        }
    })
}

pub fn finish_active_step(step: ActiveStep, status: &str, error: Option<String>) {
    finish_step(step, status, error);
}

/// Pop the topmost active step (if its frame is the current one) and
/// record an explicit completion status. Used when an error boundary
/// rewrites or absorbs an in-flight error so the receipt log reflects the
/// outcome the persona actually saw.
pub fn pop_and_record(current_frame_depth: usize, status: &str, error: Option<String>) -> bool {
    let popped = STEP_STACK.with(|stack| {
        let mut stack = stack.borrow_mut();
        if stack
            .last()
            .map(|step| step.frame_depth == current_frame_depth)
            .unwrap_or(false)
        {
            stack.pop()
        } else {
            None
        }
    });
    let Some(step) = popped else {
        return false;
    };
    finish_step(step, status, error);
    true
}

fn finish_step(step: ActiveStep, status: &str, error: Option<String>) {
    let summary = CompletedStep {
        name: step.definition.name.clone(),
        function: step.definition.function.clone(),
        model: step
            .last_model
            .clone()
            .or_else(|| step.definition.model.clone()),
        input_tokens: step.input_tokens,
        output_tokens: step.output_tokens,
        cost_usd: step.cost_usd,
        llm_calls: step.llm_calls,
        status: status.to_string(),
        error,
    };
    COMPLETED_STEPS.with(|completed| completed.borrow_mut().push(summary));
}

/// Get a snapshot of the topmost active step, if any. Used by the
/// llm_call path to fill in defaults — never for mutation.
pub fn with_active_step<R>(f: impl FnOnce(&ActiveStep) -> R) -> Option<R> {
    STEP_STACK.with(|stack| stack.borrow().last().map(f))
}

/// Mutate the topmost active step (typically to attribute LLM usage).
pub fn with_active_step_mut<R>(f: impl FnOnce(&mut ActiveStep) -> R) -> Option<R> {
    STEP_STACK.with(|stack| stack.borrow_mut().last_mut().map(f))
}

/// Frame depth of the topmost active step, or `None` when no step is
/// active. Used by `handle_error` to detect "this throw is exiting a
/// step's frame".
pub fn active_step_frame_depth() -> Option<usize> {
    STEP_STACK.with(|stack| stack.borrow().last().map(|s| s.frame_depth))
}

/// Default model the topmost active step should impose on `llm_call`
/// invocations whose options dict didn't pin a model.
pub fn active_step_model_default() -> Option<String> {
    STEP_STACK.with(|stack| {
        stack
            .borrow()
            .last()
            .and_then(|step| step.definition.model.clone())
    })
}

/// Record that `llm_call` consumed `input_tokens` / `output_tokens` for
/// `cost_usd`. Updates the active step's running totals and returns a
/// budget-exhaustion error if the step's ceiling is now breached.
///
/// The check is performed AFTER the call so the test fixture's first
/// call (which fits under budget) succeeds and subsequent calls trip the
/// limit. This matches the existing `accumulate_cost_for_provider`
/// pattern where global budget is also checked post-hoc.
pub fn record_step_llm_usage(
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
    cost_usd: f64,
) -> Result<(), VmError> {
    let exhausted = STEP_STACK.with(|stack| -> Option<VmError> {
        let mut stack = stack.borrow_mut();
        let step = stack.last_mut()?;
        step.input_tokens = step.input_tokens.saturating_add(input_tokens.max(0) as u64);
        step.output_tokens = step
            .output_tokens
            .saturating_add(output_tokens.max(0) as u64);
        step.cost_usd += cost_usd;
        step.llm_calls = step.llm_calls.saturating_add(1);
        if !model.is_empty() {
            step.last_model = Some(model.to_string());
        }

        if let Some(max_tokens) = step.definition.max_tokens {
            if step.total_tokens() > max_tokens {
                return Some(budget_exhausted_error(
                    &step.definition,
                    "max_tokens",
                    max_tokens as f64,
                    step.total_tokens() as f64,
                    step.cost_usd,
                ));
            }
        }
        if let Some(max_usd) = step.definition.max_usd {
            if step.cost_usd > max_usd {
                return Some(budget_exhausted_error(
                    &step.definition,
                    "max_usd",
                    max_usd,
                    step.total_tokens() as f64,
                    step.cost_usd,
                ));
            }
        }
        None
    });
    if let Some(err) = exhausted {
        return Err(err);
    }
    Ok(())
}

fn budget_exhausted_error(
    definition: &StepDefinition,
    limit: &str,
    limit_value: f64,
    consumed_tokens: f64,
    consumed_cost_usd: f64,
) -> VmError {
    let mut dict: BTreeMap<String, VmValue> = BTreeMap::new();
    dict.insert(
        "category".to_string(),
        VmValue::String(Rc::from("budget_exceeded")),
    );
    dict.insert(
        "kind".to_string(),
        VmValue::String(Rc::from("budget_exhausted")),
    );
    dict.insert(
        "reason".to_string(),
        VmValue::String(Rc::from("step_budget_exhausted")),
    );
    dict.insert(
        "step".to_string(),
        VmValue::String(Rc::from(definition.name.clone())),
    );
    dict.insert(
        "function".to_string(),
        VmValue::String(Rc::from(definition.function.clone())),
    );
    dict.insert(
        "limit".to_string(),
        VmValue::String(Rc::from(limit.to_string())),
    );
    dict.insert("limit_value".to_string(), VmValue::Float(limit_value));
    dict.insert(
        "consumed_tokens".to_string(),
        VmValue::Float(consumed_tokens),
    );
    dict.insert(
        "consumed_cost_usd".to_string(),
        VmValue::Float(consumed_cost_usd),
    );
    dict.insert(
        "error_boundary".to_string(),
        VmValue::String(Rc::from(
            definition
                .error_boundary
                .clone()
                .unwrap_or_else(|| "fail".to_string()),
        )),
    );
    dict.insert(
        "message".to_string(),
        VmValue::String(Rc::from(format!(
            "step `{}` exceeded {} budget ({} > {})",
            definition.name, limit, consumed_tokens as i64, limit_value as i64
        ))),
    );
    VmError::Thrown(VmValue::Dict(Rc::new(dict)))
}

/// Returns true if the thrown value looks like a budget-exhausted
/// error — either our typed step-budget dict or the existing
/// `crates/harn-vm/src/llm/cost.rs::budget_exceeded_error` shape.
/// Either form is treated identically by `error_boundary` because the
/// per-step budget machinery layers onto the existing envelope; a step
/// whose budget the preflight projection rejects is still a budget
/// exhaustion the step authored.
pub fn is_step_budget_exhausted(err: &VmError) -> bool {
    let VmError::Thrown(VmValue::Dict(dict)) = err else {
        return false;
    };
    let category = dict.get("category").and_then(vm_str);
    let kind = dict.get("kind").and_then(vm_str);
    let reason = dict.get("reason").and_then(vm_str);
    if matches!(kind, Some("budget_exhausted")) && matches!(reason, Some("step_budget_exhausted")) {
        return true;
    }
    matches!(category, Some("budget_exceeded"))
}

/// Annotate an existing budget-exhausted error with `escalated: true`
/// and the step's identity so the persona body / handoff receiver can
/// route on it. Returns the original error if it isn't a thrown dict.
/// Ensures `step` and `function` keys reflect the just-finished step
/// even when the underlying error was raised by the preflight budget
/// machinery (which doesn't know which step it's running under).
pub fn mark_escalated(err: VmError, step_name: Option<&str>, function: Option<&str>) -> VmError {
    let VmError::Thrown(VmValue::Dict(dict)) = err else {
        return err;
    };
    let mut next = (*dict).clone();
    next.insert("escalated".to_string(), VmValue::Bool(true));
    next.insert(
        "category".to_string(),
        VmValue::String(Rc::from("handoff_escalation")),
    );
    if let Some(step) = step_name {
        next.entry("step".to_string())
            .or_insert_with(|| VmValue::String(Rc::from(step.to_string())));
    }
    if let Some(function) = function {
        next.entry("function".to_string())
            .or_insert_with(|| VmValue::String(Rc::from(function.to_string())));
    }
    VmError::Thrown(VmValue::Dict(Rc::new(next)))
}

/// Drain the completed-step log. Used by receipt builders that want a
/// per-step model + token + cost breakdown for the just-finished run.
pub fn drain_completed_steps() -> Vec<CompletedStep> {
    COMPLETED_STEPS.with(|completed| std::mem::take(&mut *completed.borrow_mut()))
}

/// Read the completed-step log without clearing it. Use when callers
/// want a peek without disturbing the global record stream.
pub fn peek_completed_steps() -> Vec<CompletedStep> {
    COMPLETED_STEPS.with(|completed| completed.borrow().clone())
}

/// Lower a [`CompletedStep`] into JSON for embedding in receipts /
/// inspect output.
pub fn completed_step_to_json(step: &CompletedStep) -> JsonValue {
    serde_json::to_value(step).unwrap_or(JsonValue::Null)
}

/// Register the `__register_step` host builtin. Compiler-emitted
/// bytecode after every `@step` declaration calls it with
/// `(function_name, metadata_dict)` so the runtime can later dispatch on
/// the step's metadata when its function is invoked.
pub fn register_step_builtins(vm: &mut crate::vm::Vm) {
    vm.register_builtin("__register_step", |args, _out| {
        register_step_from_dict(args.to_vec())
    });
    vm.register_builtin("__register_persona", |args, _out| {
        register_persona_from_dict(args.to_vec())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_state() {
        reset_thread_local_state();
    }

    #[test]
    fn registers_and_pops_step_from_dict() {
        fresh_state();
        let mut budget: BTreeMap<String, VmValue> = BTreeMap::new();
        budget.insert("max_tokens".to_string(), VmValue::Int(100));
        budget.insert("max_usd".to_string(), VmValue::Float(0.05));
        let mut meta: BTreeMap<String, VmValue> = BTreeMap::new();
        meta.insert("name".to_string(), VmValue::String(Rc::from("plan")));
        meta.insert(
            "model".to_string(),
            VmValue::String(Rc::from("claude-haiku-4-5")),
        );
        meta.insert(
            "error_boundary".to_string(),
            VmValue::String(Rc::from("continue")),
        );
        meta.insert("budget".to_string(), VmValue::Dict(Rc::new(budget)));

        register_step_from_dict(vec![
            VmValue::String(Rc::from("plan_step")),
            VmValue::Dict(Rc::new(meta)),
        ])
        .expect("registration succeeds");

        assert!(maybe_push_active_step("plan_step", 3, &[]));
        assert_eq!(active_step_frame_depth(), Some(3));
        assert_eq!(
            active_step_model_default().as_deref(),
            Some("claude-haiku-4-5")
        );

        record_step_llm_usage("claude-haiku-4-5", 10, 20, 0.001).expect("under budget");
        with_active_step(|step| {
            assert_eq!(step.input_tokens, 10);
            assert_eq!(step.output_tokens, 20);
            assert!((step.cost_usd - 0.001).abs() < 1e-9);
        });

        let err =
            record_step_llm_usage("claude-haiku-4-5", 50, 50, 0.0).expect_err("should exhaust");
        assert!(is_step_budget_exhausted(&err));

        prune_below_frame(2);
        let completed = drain_completed_steps();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].llm_calls, 2);
    }

    #[test]
    fn unregistered_function_does_not_push() {
        fresh_state();
        assert!(!maybe_push_active_step("not_a_step", 1, &[]));
        assert!(active_step_frame_depth().is_none());
    }
}
