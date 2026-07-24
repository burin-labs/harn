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

use crate::value::VmDictExt;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::orchestration::{
    current_execution_policy, pop_execution_policy, push_execution_policy, CapabilityPolicy,
    HookEvent,
};
use crate::personas::StageDecl;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
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
    /// Per-stage tool/side-effect scoping. Keyed lookups by stage name happen
    /// every step entry; the list is small (a handful of stages per persona)
    /// so a `Vec` keeps insertion order and matches the manifest's authored
    /// ordering.
    pub stages: Vec<StageDecl>,
    /// The persona's declared output style (how it shapes its prose), surfaced
    /// to Harn via `persona_output_style()`. `None` when the persona declares
    /// no style.
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

/// Tracks one in-flight step. The `frame_depth` is `Vm::frames.len()`
/// captured immediately after `push_closure_frame` returns, so an
/// `ActiveStep` is "alive" while `Vm::frames.len() >= frame_depth`.
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
    /// Tracing span id opened when the step's frame was pushed; ended on
    /// completion. 0 when tracing was disabled at push time, in which
    /// case `span_end` is a no-op anyway.
    pub span_id: u64,
    /// True when this step pushed a per-stage `CapabilityPolicy` onto the
    /// execution policy stack. The runtime pops it when the step's frame
    /// unwinds, mirroring the RAII guard pattern in
    /// `crates/harn-serve/src/adapters/acp/modes.rs`.
    pub stage_policy_pushed: bool,
}

impl ActiveStep {
    fn new(
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

    fn total_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
}

#[derive(Debug, Clone)]
pub struct ActivePersona {
    pub frame_depth: usize,
    pub definition: Arc<PersonaDefinition>,
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
    static STEP_REGISTRY: RefCell<BTreeMap<String, Arc<StepDefinition>>> =
        const { RefCell::new(std::collections::BTreeMap::new()) };
    static PERSONA_REGISTRY: RefCell<BTreeMap<String, Arc<PersonaDefinition>>> =
        const { RefCell::new(std::collections::BTreeMap::new()) };
    static STEP_REGISTRY_LEN: Cell<usize> = const { Cell::new(0) };
    static PERSONA_REGISTRY_LEN: Cell<usize> = const { Cell::new(0) };
    static PERSONA_STACK: RefCell<Vec<ActivePersona>> = const { RefCell::new(Vec::new()) };
    static STEP_STACK: RefCell<Vec<ActiveStep>> = const { RefCell::new(Vec::new()) };
    static ACTIVE_CONTEXT_SUSPENSION_STACK: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
    static COMPLETED_STEPS: RefCell<Vec<CompletedStep>> = const { RefCell::new(Vec::new()) };
    static PERSONA_HOOKS: RefCell<Vec<PersonaHookRegistration>> = const { RefCell::new(Vec::new()) };
}

/// Reset every thread-local owned by this module. Called between test
/// runs and at the start of each top-level program execution so leftover
/// registrations don't leak across runs.
pub fn reset_thread_local_state() {
    STEP_REGISTRY.with(|r| r.borrow_mut().clear());
    PERSONA_REGISTRY.with(|r| r.borrow_mut().clear());
    STEP_REGISTRY_LEN.with(|len| len.set(0));
    PERSONA_REGISTRY_LEN.with(|len| len.set(0));
    PERSONA_STACK.with(|s| s.borrow_mut().clear());
    STEP_STACK.with(|s| s.borrow_mut().clear());
    ACTIVE_CONTEXT_SUSPENSION_STACK.with(|s| s.borrow_mut().clear());
    COMPLETED_STEPS.with(|c| c.borrow_mut().clear());
    PERSONA_HOOKS.with(|h| h.borrow_mut().clear());
}

#[inline]
fn step_registry_empty() -> bool {
    STEP_REGISTRY_LEN.with(|len| len.get() == 0)
}

#[inline]
fn persona_registry_empty() -> bool {
    PERSONA_REGISTRY_LEN.with(|len| len.get() == 0)
}

#[inline]
fn tracked_registries_empty() -> bool {
    step_registry_empty() && persona_registry_empty()
}

/// Bind a `@step` function name to its declared metadata. Idempotent: a
/// second call replaces the prior definition (matches re-evaluation
/// semantics of `harn run` and the conformance harness).
pub fn register_step(function: &str, definition: StepDefinition) {
    let inserted = STEP_REGISTRY.with(|registry| {
        registry
            .borrow_mut()
            .insert(function.to_string(), Arc::new(definition))
            .is_none()
    });
    if inserted {
        STEP_REGISTRY_LEN.with(|len| len.set(len.get() + 1));
    }
}

pub fn register_persona(function: &str, definition: PersonaDefinition) {
    let inserted = PERSONA_REGISTRY.with(|registry| {
        registry
            .borrow_mut()
            .insert(function.to_string(), Arc::new(definition))
            .is_none()
    });
    if inserted {
        PERSONA_REGISTRY_LEN.with(|len| len.set(len.get() + 1));
    }
}

pub fn register_persona_from_dict(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let function = args
        .first()
        .and_then(vm_str)
        .map(|s| s.to_string())
        .ok_or_else(|| {
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                "__register_persona: expected (function_name, metadata_dict)",
            )))
        })?;
    let meta = args
        .get(1)
        .and_then(VmValue::as_dict)
        .cloned()
        .ok_or_else(|| {
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                "__register_persona: metadata argument must be a dict",
            )))
        })?;
    let definition = PersonaDefinition {
        name: meta
            .get("name")
            .and_then(vm_str)
            .map(str::to_string)
            .unwrap_or_else(|| function.clone()),
        stages: parse_stage_decls(meta.get("stages"))?,
        output_style: parse_output_style(meta.get("output_style")),
    };
    register_persona(&function, definition);
    Ok(VmValue::Nil)
}

/// Parse an `output_style` metadata value into a [`PersonaOutputStyle`].
/// Accepts a bare string (a named style) or a dict with `name`/`instructions`.
/// Returns `None` for nil or an empty style.
fn parse_output_style(
    value: Option<&VmValue>,
) -> Option<harn_modules::personas::PersonaOutputStyle> {
    use harn_modules::personas::PersonaOutputStyle;
    let style = match value? {
        VmValue::Nil => return None,
        VmValue::String(name) => PersonaOutputStyle::from_name(name.to_string()),
        VmValue::Dict(_) => {
            let dict = value?.as_dict()?;
            PersonaOutputStyle {
                name: dict.get("name").and_then(vm_str).map(str::to_string),
                instructions: dict
                    .get("instructions")
                    .and_then(vm_str)
                    .map(str::to_string),
            }
        }
        _ => return None,
    };
    (!style.is_empty()).then_some(style)
}

/// Build the Harn dict shape for a persona output style.
fn output_style_to_vm(style: &harn_modules::personas::PersonaOutputStyle) -> VmValue {
    use crate::value::{intern_key, DictMap};
    let mut map = DictMap::new();
    map.insert(
        intern_key("name"),
        style
            .name
            .as_deref()
            .map(|name| VmValue::String(arcstr::ArcStr::from(name)))
            .unwrap_or(VmValue::Nil),
    );
    map.insert(
        intern_key("instructions"),
        style
            .instructions
            .as_deref()
            .map(|text| VmValue::String(arcstr::ArcStr::from(text)))
            .unwrap_or(VmValue::Nil),
    );
    VmValue::dict(map)
}

/// Look up a persona's declared output style. With no argument (or nil), reads
/// the currently-active persona (top of the persona stack); with a persona
/// function name, reads that persona from the registry. Returns
/// `{name, instructions}` or nil.
pub fn persona_output_style(args: Vec<VmValue>) -> VmValue {
    if let Some(function) = args.first().and_then(vm_str) {
        return PERSONA_REGISTRY.with(|registry| {
            registry
                .borrow()
                .get(function)
                .and_then(|definition| definition.output_style.as_ref().map(output_style_to_vm))
                .unwrap_or(VmValue::Nil)
        });
    }
    PERSONA_STACK.with(|stack| {
        stack
            .borrow()
            .last()
            .and_then(|active| {
                active
                    .definition
                    .output_style
                    .as_ref()
                    .map(output_style_to_vm)
            })
            .unwrap_or(VmValue::Nil)
    })
}

fn parse_stage_decls(value: Option<&VmValue>) -> Result<Vec<StageDecl>, VmError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let entries = match value {
        VmValue::Nil => return Ok(Vec::new()),
        VmValue::List(list) => list.as_ref(),
        _ => {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                "__register_persona: stages argument must be a list of dicts",
            ))));
        }
    };
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let dict = entry.as_dict().ok_or_else(|| {
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                "__register_persona: each stage entry must be a dict",
            )))
        })?;
        let Some(name) = dict.get("name").and_then(vm_str) else {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                "__register_persona: stage dict missing required 'name'",
            ))));
        };
        let allowed_tools = match dict.get("allowed_tools") {
            None | Some(VmValue::Nil) => None,
            Some(VmValue::List(items)) => Some(
                items
                    .iter()
                    .map(|item| {
                        vm_str(item).map(str::to_string).ok_or_else(|| {
                            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                                "__register_persona: stage allowed_tools entries must be strings",
                            )))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            _ => {
                return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    "__register_persona: stage allowed_tools must be a list of strings",
                ))));
            }
        };
        let side_effect_level = dict
            .get("side_effect_level")
            .and_then(vm_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty());
        let max_iterations = match dict.get("max_iterations") {
            Some(VmValue::Int(n)) if *n >= 0 => Some(*n as u32),
            Some(VmValue::Float(f)) if f.is_finite() && *f >= 0.0 => Some(*f as u32),
            _ => None,
        };
        out.push(StageDecl {
            name: name.to_string(),
            allowed_tools,
            side_effect_level,
            max_iterations,
            on_exit: None,
        });
    }
    Ok(out)
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
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                "__register_step: expected (function_name, metadata_dict)",
            )))
        })?;
    let meta = args
        .get(1)
        .and_then(VmValue::as_dict)
        .cloned()
        .ok_or_else(|| {
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
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
    pub handler: Arc<VmClosure>,
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
    pub handler: Arc<VmClosure>,
    pub event: HookEvent,
}

pub fn register_persona_hook(
    persona_pattern: impl Into<String>,
    event: HookEvent,
    threshold_pct: Option<f64>,
    handler: Arc<VmClosure>,
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
    handler: Arc<VmClosure>,
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

#[derive(Clone, Default)]
pub(crate) struct ActiveContextSnapshot {
    steps: Vec<ActiveStep>,
    personas: Vec<ActivePersona>,
}

pub(crate) fn swap_active_context(snapshot: ActiveContextSnapshot) -> ActiveContextSnapshot {
    ActiveContextSnapshot {
        steps: STEP_STACK.with(|stack| std::mem::replace(&mut *stack.borrow_mut(), snapshot.steps)),
        personas: PERSONA_STACK
            .with(|stack| std::mem::replace(&mut *stack.borrow_mut(), snapshot.personas)),
    }
}

pub(crate) fn swap_active_context_suspension_stack(next: Vec<u64>) -> Vec<u64> {
    ACTIVE_CONTEXT_SUSPENSION_STACK.with(|stack| std::mem::replace(&mut *stack.borrow_mut(), next))
}

static NEXT_ACTIVE_CONTEXT_SUSPENSION_ID: AtomicU64 = AtomicU64::new(1);

/// Temporarily clear the active step/persona context and restore it on drop.
///
/// The guard is deliberately held across callback/module futures. If one of
/// those futures is cancelled, its drop path restores the caller's context
/// instead of stranding the empty or nested context in the thread-local slots.
pub(crate) fn suspend_active_context() -> ActiveContextGuard {
    let id = NEXT_ACTIVE_CONTEXT_SUSPENSION_ID.fetch_add(1, Ordering::Relaxed);
    ACTIVE_CONTEXT_SUSPENSION_STACK.with(|stack| stack.borrow_mut().push(id));
    ActiveContextGuard {
        id,
        outer: Some(swap_active_context(ActiveContextSnapshot::default())),
    }
}

pub(crate) struct ActiveContextGuard {
    id: u64,
    outer: Option<ActiveContextSnapshot>,
}

impl Drop for ActiveContextGuard {
    fn drop(&mut self) {
        let owns_current_scope = ACTIVE_CONTEXT_SUSPENSION_STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            if stack.last() == Some(&self.id) {
                stack.pop();
                true
            } else {
                false
            }
        });
        if owns_current_scope {
            let outer = self.outer.take().expect("active context snapshot");
            let _ = swap_active_context(outer);
        }
    }
}

pub fn is_tracked_function(function_name: &str) -> bool {
    if tracked_registries_empty() {
        return false;
    }
    (!step_registry_empty()
        && STEP_REGISTRY.with(|registry| registry.borrow().contains_key(function_name)))
        || (!persona_registry_empty()
            && PERSONA_REGISTRY.with(|registry| registry.borrow().contains_key(function_name)))
}

pub fn step_definition_for_function(function_name: &str) -> Option<Arc<StepDefinition>> {
    if step_registry_empty() {
        return None;
    }
    STEP_REGISTRY.with(|registry| registry.borrow().get(function_name).cloned())
}

pub fn current_persona_name() -> Option<String> {
    PERSONA_STACK.with(|stack| stack.borrow().last().map(|p| p.definition.name.clone()))
}

/// Resolve the per-stage policy for `step_name` against the currently
/// active persona's stage declarations. Returns `None` when no persona is
/// active or no stage matches the step name. Caller pushes the result onto
/// `EXECUTION_POLICY_STACK`.
///
/// When an ambient policy is already active, the stage policy is
/// intersected with it so a stage can only ever tighten the tool surface
/// and side-effect ceiling — never widen them.
fn stage_policy_for_active_step(step_name: &str) -> Option<CapabilityPolicy> {
    let stage_policy = PERSONA_STACK.with(|stack| {
        let stack = stack.borrow();
        let persona = stack.last()?;
        let stage = persona
            .definition
            .stages
            .iter()
            .find(|stage| stage.name == step_name)?;
        Some(stage_decl_to_policy(stage))
    })?;
    let Some(parent) = current_execution_policy() else {
        return Some(stage_policy);
    };
    let mut stage_policy = stage_policy;
    if stage_policy.tools_are_restricted() {
        let tools = stage_policy
            .tools
            .iter()
            .filter(|tool| parent.tool_pattern_allows(tool))
            .cloned()
            .collect();
        stage_policy.restrict_tools(tools);
    }
    Some(
        parent
            .intersect(&stage_policy)
            .expect("pre-narrowed stage policy must fit the parent ceiling"),
    )
}

fn stage_decl_to_policy(stage: &StageDecl) -> CapabilityPolicy {
    let mut policy = CapabilityPolicy {
        side_effect_level: stage.side_effect_level.clone(),
        ..CapabilityPolicy::default()
    };
    if let Some(tools) = &stage.allowed_tools {
        policy.restrict_tools(tools.clone());
    }
    policy
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
    if persona_registry_empty() {
        return false;
    }
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
    if step_registry_empty() {
        return false;
    }
    let definition = STEP_REGISTRY.with(|registry| registry.borrow().get(function_name).cloned());
    let Some(definition) = definition else {
        return false;
    };
    let persona = current_persona_name();
    let span_id =
        crate::tracing::span_start(crate::tracing::SpanKind::Step, definition.name.clone());
    if let Some(persona_name) = persona.as_deref() {
        crate::tracing::span_set_metadata(
            span_id,
            "persona",
            serde_json::Value::String(persona_name.to_string()),
        );
    }
    if let Some(model) = definition.model.as_deref() {
        crate::tracing::span_set_metadata(
            span_id,
            "model",
            serde_json::Value::String(model.to_string()),
        );
    }
    let step_name = definition.name.clone();
    STEP_STACK.with(|stack| {
        stack.borrow_mut().push(ActiveStep::new(
            frame_depth,
            definition,
            persona,
            args.to_vec(),
            span_id,
            false,
        ));
    });
    if let Some(policy) = stage_policy_for_active_step(&step_name) {
        push_execution_policy(policy);
        STEP_STACK.with(|stack| {
            if let Some(top) = stack.borrow_mut().last_mut() {
                top.stage_policy_pushed = true;
            }
        });
    }
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
    if step.stage_policy_pushed {
        pop_execution_policy();
    }
    crate::tracing::span_set_metadata(
        step.span_id,
        "status",
        serde_json::Value::String(status.to_string()),
    );
    crate::tracing::span_set_metadata(
        step.span_id,
        "llm_calls",
        serde_json::Value::Number(step.llm_calls.into()),
    );
    crate::tracing::span_set_metadata(
        step.span_id,
        "input_tokens",
        serde_json::Value::Number(step.input_tokens.into()),
    );
    crate::tracing::span_set_metadata(
        step.span_id,
        "output_tokens",
        serde_json::Value::Number(step.output_tokens.into()),
    );
    if let Some(cost_n) = serde_json::Number::from_f64(step.cost_usd) {
        crate::tracing::span_set_metadata(
            step.span_id,
            "cost_usd",
            serde_json::Value::Number(cost_n),
        );
    }
    crate::tracing::span_end(step.span_id);
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
    let mut dict: crate::value::DictMap = crate::value::DictMap::new();
    dict.put_str("category", "budget_exceeded");
    dict.put_str("kind", "budget_exhausted");
    dict.put_str("reason", "step_budget_exhausted");
    dict.put_str("step", definition.name.clone());
    dict.put_str("function", definition.function.clone());
    dict.put_str("limit", limit);
    dict.insert(
        crate::value::intern_key("limit_value"),
        VmValue::Float(limit_value),
    );
    dict.insert(
        crate::value::intern_key("consumed_tokens"),
        VmValue::Float(consumed_tokens),
    );
    dict.insert(
        crate::value::intern_key("consumed_cost_usd"),
        VmValue::Float(consumed_cost_usd),
    );
    dict.put_str(
        "error_boundary",
        definition
            .error_boundary
            .clone()
            .unwrap_or_else(|| "fail".to_string()),
    );
    dict.put_str(
        "message",
        format!(
            "step `{}` exceeded {} budget ({} > {})",
            definition.name, limit, consumed_tokens as i64, limit_value as i64
        ),
    );
    VmError::Thrown(VmValue::dict(dict))
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
    next.insert(crate::value::intern_key("escalated"), VmValue::Bool(true));
    next.put_str("category", "handoff_escalation");
    if let Some(step) = step_name {
        next.entry(crate::value::intern_key("step"))
            .or_insert_with(|| VmValue::String(arcstr::ArcStr::from(step.to_string())));
    }
    if let Some(function) = function {
        next.entry(crate::value::intern_key("function"))
            .or_insert_with(|| VmValue::String(arcstr::ArcStr::from(function.to_string())));
    }
    VmError::Thrown(VmValue::dict(next))
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

/// Register the `__register_step` and `__register_persona` host builtins.
/// Compiler-emitted bytecode after every `@step` / persona declaration
/// calls these with `(function_name, metadata_dict)` so the runtime can
/// later dispatch on the step's metadata when its function is invoked.
pub fn register_step_builtins(vm: &mut crate::vm::Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &__REGISTER_STEP_DEF,
    &__REGISTER_PERSONA_DEF,
    &__PERSONA_OUTPUT_STYLE_DEF,
];

#[harn_builtin(category = "step_runtime", runtime_only = true)]
fn __register_step(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    register_step_from_dict(args.to_vec())
}

#[harn_builtin(category = "step_runtime", runtime_only = true)]
fn __register_persona(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    register_persona_from_dict(args.to_vec())
}

#[harn_builtin(
    sig = "__persona_output_style(function?: string) -> dict",
    category = "step_runtime",
    runtime_only = true
)]
fn __persona_output_style(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(persona_output_style(args.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::VmDictExt;

    fn fresh_state() {
        reset_thread_local_state();
    }

    #[test]
    fn persona_output_style_reads_registry_and_active_stack() {
        use crate::value::{intern_key, DictMap};
        fresh_state();

        // Register a persona whose metadata carries a table-form output style.
        let mut style = DictMap::new();
        style.put_str("name", "concise");
        style.put_str("instructions", "Be terse.");
        let mut meta = DictMap::new();
        meta.put_str("name", "Reviewer");
        meta.insert(intern_key("output_style"), VmValue::dict(style));
        register_persona_from_dict(vec![
            VmValue::String(arcstr::ArcStr::from("reviewer_fn")),
            VmValue::dict(meta),
        ])
        .expect("persona registers");

        // Lookup by function name returns the declared style.
        let by_name =
            persona_output_style(vec![VmValue::String(arcstr::ArcStr::from("reviewer_fn"))]);
        let dict = by_name.as_dict().expect("dict");
        assert_eq!(
            dict.get("name").map(VmValue::display).as_deref(),
            Some("concise")
        );
        assert_eq!(
            dict.get("instructions").map(VmValue::display).as_deref(),
            Some("Be terse.")
        );

        // No active persona on the stack → nil.
        assert!(matches!(persona_output_style(vec![]), VmValue::Nil));
        // Unknown persona → nil.
        assert!(matches!(
            persona_output_style(vec![VmValue::String(arcstr::ArcStr::from("nope"))]),
            VmValue::Nil
        ));
    }

    #[test]
    fn registers_and_pops_step_from_dict() {
        fresh_state();
        let mut budget: crate::value::DictMap = crate::value::DictMap::new();
        budget.insert(crate::value::intern_key("max_tokens"), VmValue::Int(100));
        budget.insert(crate::value::intern_key("max_usd"), VmValue::Float(0.05));
        let mut meta: crate::value::DictMap = crate::value::DictMap::new();
        meta.put_str("name", "plan");
        meta.put_str("model", "claude-haiku-4-5");
        meta.put_str("error_boundary", "continue");
        meta.insert(crate::value::intern_key("budget"), VmValue::dict(budget));

        register_step_from_dict(vec![
            VmValue::String(arcstr::ArcStr::from("plan_step")),
            VmValue::dict(meta),
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

    #[test]
    fn tracked_registry_empty_fast_path_tracks_registrations_and_reset() {
        fresh_state();
        assert!(tracked_registries_empty());
        assert!(!is_tracked_function("plan_step"));

        register_step(
            "plan_step",
            StepDefinition {
                name: "plan".to_string(),
                function: "plan_step".to_string(),
                ..StepDefinition::default()
            },
        );
        assert!(!tracked_registries_empty());
        assert!(is_tracked_function("plan_step"));
        assert!(step_definition_for_function("plan_step").is_some());

        register_step(
            "plan_step",
            StepDefinition {
                name: "plan_v2".to_string(),
                function: "plan_step".to_string(),
                ..StepDefinition::default()
            },
        );
        assert!(is_tracked_function("plan_step"));

        fresh_state();
        assert!(tracked_registries_empty());
        assert!(!is_tracked_function("plan_step"));
    }

    #[test]
    fn stage_policy_narrows_but_does_not_widen_parent_policy() {
        fresh_state();
        let mut meta: crate::value::DictMap = crate::value::DictMap::new();
        meta.put_str("name", "research");
        register_step_from_dict(vec![
            VmValue::String(arcstr::ArcStr::from("research_step")),
            VmValue::dict(meta),
        ])
        .expect("step registration");

        let mut stage_dict: crate::value::DictMap = crate::value::DictMap::new();
        stage_dict.put_str("name", "research");
        // Stage tries to add `edit` on top of a parent that only allowed `read`.
        stage_dict.insert(
            crate::value::intern_key("allowed_tools"),
            VmValue::List(std::sync::Arc::new(vec![
                VmValue::String(arcstr::ArcStr::from("read")),
                VmValue::String(arcstr::ArcStr::from("edit")),
            ])),
        );
        let mut persona_meta: crate::value::DictMap = crate::value::DictMap::new();
        persona_meta.put_str("name", "scoped");
        persona_meta.insert(
            crate::value::intern_key("stages"),
            VmValue::List(std::sync::Arc::new(vec![VmValue::Dict(
                std::sync::Arc::new(stage_dict),
            )])),
        );
        register_persona_from_dict(vec![
            VmValue::String(arcstr::ArcStr::from("scoped_persona")),
            VmValue::dict(persona_meta),
        ])
        .expect("persona registration");

        push_execution_policy(CapabilityPolicy {
            tools: vec!["read".to_string()],
            capabilities: std::collections::BTreeMap::from([(
                "workspace".to_string(),
                vec!["read_text".to_string()],
            )]),
            workspace_roots: vec!["/workspace".to_string()],
            side_effect_level: Some("workspace_read".to_string()),
            ..CapabilityPolicy::default()
        });
        assert!(maybe_push_active_persona("scoped_persona", 1));
        assert!(maybe_push_active_step("research_step", 2, &[]));
        let policy = current_execution_policy().expect("stage policy active");
        // `edit` is filtered out because the parent already denied it.
        assert_eq!(policy.tools, vec!["read".to_string()]);
        assert_eq!(
            policy.capabilities,
            std::collections::BTreeMap::from([(
                "workspace".to_string(),
                vec!["read_text".to_string()],
            )])
        );
        assert_eq!(policy.workspace_roots, vec!["/workspace".to_string()]);
        assert_eq!(policy.side_effect_level.as_deref(), Some("workspace_read"));

        prune_below_frame(0);
        pop_execution_policy();
        assert!(current_execution_policy().is_none());
    }

    #[test]
    fn explicit_empty_stage_tool_list_denies_every_tool() {
        let policy = stage_decl_to_policy(&StageDecl {
            name: "observe".to_string(),
            allowed_tools: Some(Vec::new()),
            ..StageDecl::default()
        });

        assert!(policy.tools_are_restricted());
        assert!(policy.tools_deny_all());
    }

    #[test]
    fn stage_policy_is_pushed_and_popped_around_step() {
        fresh_state();
        let mut meta: crate::value::DictMap = crate::value::DictMap::new();
        meta.put_str("name", "research");
        register_step_from_dict(vec![
            VmValue::String(arcstr::ArcStr::from("research_step")),
            VmValue::dict(meta),
        ])
        .expect("step registration succeeds");

        let mut stage_dict: crate::value::DictMap = crate::value::DictMap::new();
        stage_dict.put_str("name", "research");
        stage_dict.insert(
            crate::value::intern_key("allowed_tools"),
            VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                arcstr::ArcStr::from("read"),
            )])),
        );
        let mut persona_meta: crate::value::DictMap = crate::value::DictMap::new();
        persona_meta.put_str("name", "scoped");
        persona_meta.insert(
            crate::value::intern_key("stages"),
            VmValue::List(std::sync::Arc::new(vec![VmValue::Dict(
                std::sync::Arc::new(stage_dict),
            )])),
        );
        register_persona_from_dict(vec![
            VmValue::String(arcstr::ArcStr::from("scoped_persona")),
            VmValue::dict(persona_meta),
        ])
        .expect("persona registration succeeds");

        assert!(maybe_push_active_persona("scoped_persona", 1));
        assert!(crate::orchestration::current_execution_policy().is_none());
        assert!(maybe_push_active_step("research_step", 2, &[]));
        let policy = crate::orchestration::current_execution_policy()
            .expect("stage policy is active inside step");
        assert_eq!(policy.tools, vec!["read".to_string()]);

        prune_below_frame(0);
        assert!(crate::orchestration::current_execution_policy().is_none());
    }
}
