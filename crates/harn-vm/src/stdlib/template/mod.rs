//! Prompt-template engine for `.harn.prompt` assets and the `render` /
//! `render_prompt` builtins.
//!
//! # Surface
//!
//! ```text
//! {{ name }}                                 interpolation
//! {{ user.name }} / {{ items[0] }}           nested path access
//! {{ name | upper | default: "anon" }}       filter pipeline
//! {{ if expr }}..{{ elif expr }}..{{ else }}..{{ end }}
//! {{ for x in xs }}..{{ else }}..{{ end }}   else = empty-iterable fallback
//! {{ for k, v in dict }}..{{ end }}
//! {{ include "partial.harn.prompt" }}
//! {{ include "partial.harn.prompt" with { x: name } }}
//! {{ section "task" }}..{{ endsection }}
//! {{# comment — stripped at parse time #}}
//! {{ raw }}..literal {{braces}}..{{ endraw }}
//! {{- x -}}                                  whitespace-trim markers
//! ```
//!
//! Back-compat: bare `{{ident}}` resolves silently to the empty fallthrough
//! (writes back the literal text on miss) — preserving the pre-v2 contract.
//! All new constructs raise `TemplateError` on parse or evaluation failure.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use crate::value::{VmError, VmValue};

mod assets;
mod ast;
mod error;
mod expr_parser;
mod filters;
mod lexer;
pub mod lint;
mod llm_context;
mod parser;
mod render;
mod sections;

#[cfg(test)]
mod tests;

use assets::parse_cached;
pub(crate) use assets::TemplateAsset;
use error::TemplateError;
pub use llm_context::{
    current_llm_render_context, pop_llm_render_context, push_llm_render_context, LlmRenderContext,
    LlmRenderContextGuard,
};
use render::{render_nodes, RenderCtx, Scope};

// Thread-local registry of recent prompt renders keyed by `prompt_id`.
// Populated by `render_with_provenance` so the DAP adapter can serve
// `burin/promptProvenance` and `burin/promptConsumers` reverse queries
// without forcing the pipeline author to pass the spans dict back up
// through the bridge. Capped at 64 renders (FIFO) to bound memory.
thread_local! {
    static PROMPT_REGISTRY: RefCell<Vec<RegisteredPrompt>> = const { RefCell::new(Vec::new()) };
    // prompt_id -> [event_index...] where the prompt was consumed by
    // an LLM call. Populated by emission sites once they thread the
    // id alongside the rendered text; read by burin/promptConsumers
    // to power the template gutter's jump-to-next-render action
    // (#106). A per-session reset is handled by reset_prompt_registry.
    static PROMPT_RENDER_INDICES: RefCell<BTreeMap<String, Vec<u64>>> =
        const { RefCell::new(BTreeMap::new()) };
    // Monotonic render ordinal driven by the prompt_mark_rendered
    // builtin (#106). A fresh thread-local counter since the IDE
    // correlates ordinals to event_indices at render time.
    static PROMPT_RENDER_ORDINAL: RefCell<u64> = const { RefCell::new(0) };
}

const PROMPT_REGISTRY_CAP: usize = 64;

#[derive(Debug, Clone)]
pub struct RegisteredPrompt {
    pub prompt_id: String,
    pub template_uri: String,
    pub rendered: String,
    pub spans: Vec<PromptSourceSpan>,
}

/// Record a provenance map in the thread-local registry and return the
/// assigned `prompt_id`. Newest entries push to the back; when the cap
/// is reached the oldest entry is dropped so the registry never grows
/// unboundedly over long sessions.
pub(crate) fn register_prompt(
    template_uri: String,
    rendered: String,
    spans: Vec<PromptSourceSpan>,
) -> String {
    let prompt_id = format!("prompt-{}", next_prompt_serial());
    PROMPT_REGISTRY.with(|reg| {
        let mut reg = reg.borrow_mut();
        if reg.len() >= PROMPT_REGISTRY_CAP {
            reg.remove(0);
        }
        reg.push(RegisteredPrompt {
            prompt_id: prompt_id.clone(),
            template_uri,
            rendered,
            spans,
        });
    });
    prompt_id
}

thread_local! {
    static PROMPT_SERIAL: RefCell<u64> = const { RefCell::new(0) };
}

fn next_prompt_serial() -> u64 {
    PROMPT_SERIAL.with(|s| {
        let mut s = s.borrow_mut();
        *s += 1;
        *s
    })
}

/// Resolve an output byte offset to its originating template span.
/// Returns the innermost matching `Expr` / `LegacyBareInterp` span when
/// one exists, falling back to broader structural spans (If / For /
/// Include) so a click anywhere in a rendered loop iteration still
/// navigates somewhere useful.
pub fn lookup_prompt_span(
    prompt_id: &str,
    output_offset: usize,
) -> Option<(String, PromptSourceSpan)> {
    PROMPT_REGISTRY.with(|reg| {
        let reg = reg.borrow();
        let entry = reg.iter().find(|p| p.prompt_id == prompt_id)?;
        let best = entry
            .spans
            .iter()
            .filter(|s| {
                output_offset >= s.output_start
                    && output_offset < s.output_end.max(s.output_start + 1)
            })
            .min_by_key(|s| {
                let width = s.output_end.saturating_sub(s.output_start);
                let kind_weight = match s.kind {
                    PromptSpanKind::Expr => 0,
                    PromptSpanKind::LegacyBareInterp => 1,
                    PromptSpanKind::Text => 2,
                    PromptSpanKind::Section => 3,
                    PromptSpanKind::Include => 4,
                    PromptSpanKind::ForIteration => 5,
                    PromptSpanKind::If => 6,
                };
                (kind_weight, width)
            })?
            .clone();
        Some((entry.template_uri.clone(), best))
    })
}

/// Return every span across every registered prompt that overlaps a
/// template range. Powers the inverse "which rendered ranges consumed
/// this template region?" navigation.
pub fn lookup_prompt_consumers(
    template_uri: &str,
    template_line_start: usize,
    template_line_end: usize,
) -> Vec<(String, PromptSourceSpan)> {
    PROMPT_REGISTRY.with(|reg| {
        let reg = reg.borrow();
        reg.iter()
            .flat_map(|p| {
                let prompt_id = p.prompt_id.clone();
                p.spans
                    .iter()
                    .filter(move |s| {
                        let line = s.template_line;
                        s.template_uri == template_uri
                            && line > 0
                            && line >= template_line_start
                            && line <= template_line_end
                    })
                    .cloned()
                    .map(move |s| (prompt_id.clone(), s))
            })
            .collect()
    })
}

/// Record a render event index against a prompt_id (#106). The
/// scrubber's jump-to-render action walks this map to move the
/// playhead to the AgentEvent where the template was consumed.
/// Stored as a Vec so re-renders of the same prompt id accumulate.
pub fn record_prompt_render_index(prompt_id: &str, event_index: u64) {
    PROMPT_RENDER_INDICES.with(|map| {
        map.borrow_mut()
            .entry(prompt_id.to_string())
            .or_default()
            .push(event_index);
    });
}

/// Produce the next monotonic ordinal for a render-mark. Pipelines
/// invoke the `prompt_mark_rendered` builtin which calls this to
/// obtain a sequence number without having to know about per-session
/// event counters. The IDE scrubber orders matching consumers by
/// this ordinal when the emitted_at_ms timestamps collide.
pub fn next_prompt_render_ordinal() -> u64 {
    PROMPT_RENDER_ORDINAL.with(|c| {
        let mut n = c.borrow_mut();
        *n += 1;
        *n
    })
}

/// Fetch every event index where `prompt_id` was rendered. Called
/// by the DAP adapter to populate the `eventIndices` list in the
/// `burin/promptConsumers` response.
pub fn prompt_render_indices(prompt_id: &str) -> Vec<u64> {
    PROMPT_RENDER_INDICES.with(|map| map.borrow().get(prompt_id).cloned().unwrap_or_default())
}

/// Clear the registry. Wired into `reset_thread_local_state` so tests
/// and serialized adapter sessions start from a clean slate.
pub(crate) fn reset_prompt_registry() {
    PROMPT_REGISTRY.with(|reg| reg.borrow_mut().clear());
    PROMPT_SERIAL.with(|s| *s.borrow_mut() = 0);
    PROMPT_RENDER_INDICES.with(|map| map.borrow_mut().clear());
    PROMPT_RENDER_ORDINAL.with(|c| *c.borrow_mut() = 0);
    llm_context::reset_llm_render_stack();
    if let Some(cache) = LLM_SHADOW_WARN_CACHE.get() {
        if let Ok(mut g) = cache.lock() {
            g.clear();
        }
    }
}

/// One-shot dedup for the user-supplied-`llm`-binding shadow warning.
/// Keyed by template URI so a recurring render in a loop only emits
/// the warning once per template per process.
static LLM_SHADOW_WARN_CACHE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// Build the merged bindings map that includes the ambient `llm` key
/// when an LLM render context is in scope. Returns `None` to mean
/// "no change required — pass the caller's bindings through unchanged":
/// either there is no active context, or the user already supplied an
/// `llm` binding (in which case we emit a lint warning and let their
/// value win for back-compat).
fn augment_bindings_with_llm(
    asset: &TemplateAsset,
    bindings: Option<&BTreeMap<String, VmValue>>,
) -> Option<BTreeMap<String, VmValue>> {
    let ctx = current_llm_render_context()?;
    if bindings.is_some_and(|m| m.contains_key("llm")) {
        warn_user_llm_shadowed(asset);
        return None;
    }
    let mut merged = bindings.cloned().unwrap_or_default();
    merged.insert("llm".to_string(), ctx.to_vm_value());
    Some(merged)
}

fn warn_user_llm_shadowed(asset: &TemplateAsset) {
    let cache = LLM_SHADOW_WARN_CACHE.get_or_init(|| Mutex::new(HashSet::new()));
    let key = asset.uri.clone();
    {
        let mut guard = match cache.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if !guard.insert(key.clone()) {
            return;
        }
    }
    crate::events::log_warn_meta(
        "template.llm_scope",
        "user-supplied `llm` binding shadows auto-injected LLM render context; \
         rename your key to avoid relying on this back-compat path",
        BTreeMap::from([
            ("template_uri".to_string(), serde_json::Value::String(key)),
            (
                "reason".to_string(),
                serde_json::Value::String("user_binding_shadowed".to_string()),
            ),
        ]),
    );
}

/// Parse-only validation for lint/preflight. Returns a human-readable error
/// message when the template body is syntactically invalid; `Ok(())` when the
/// template would parse. Does not resolve `{{ include }}` targets — those are
/// validated at render time with their own error reporting.
pub fn validate_template_syntax(src: &str) -> Result<(), String> {
    parser::parse(src).map(|_| ()).map_err(|e| e.message())
}

/// Full-featured entrypoint that preserves errors. `base` is the directory
/// used to resolve `{{ include "..." }}` paths; `source_path` (if known) is
/// included in error messages.
pub(crate) fn render_template_result(
    template: &str,
    bindings: Option<&BTreeMap<String, VmValue>>,
    base: Option<&Path>,
    source_path: Option<&Path>,
) -> Result<String, TemplateError> {
    let (rendered, _spans) =
        render_template_with_provenance(template, bindings, base, source_path, false)?;
    Ok(rendered)
}

/// Render a template for callers outside the VM crate that need the same
/// prompt-template semantics as `render(...)` / `render_prompt(...)`.
pub fn render_template_to_string(
    template: &str,
    bindings: Option<&BTreeMap<String, VmValue>>,
    base: Option<&Path>,
    source_path: Option<&Path>,
) -> Result<String, String> {
    render_template_result(template, bindings, base, source_path).map_err(|error| error.message())
}

/// One byte-range in a rendered prompt mapped back to its source
/// template. Foundation for the prompt-provenance UX (burin-code #93):
/// hover a chunk of the live prompt in the debugger and jump to the
/// `.harn.prompt` line that produced it.
///
/// `output_start` / `output_end` are byte offsets into the rendered
/// string. `template_line` / `template_col` are 1-based positions in
/// the source template. `bound_value` carries a short preview of the
/// expression's runtime value when it's a scalar; omitted for
/// structural nodes (if/for/include) so callers don't log a giant
/// dict display for a single `{% for %}`.
#[derive(Debug, Clone)]
pub struct PromptSourceSpan {
    pub template_line: usize,
    pub template_col: usize,
    pub output_start: usize,
    pub output_end: usize,
    pub kind: PromptSpanKind,
    pub bound_value: Option<String>,
    /// When the span was rendered from inside an `include` (possibly
    /// transitively), this points at the including call's span in the
    /// parent template. Chained boxes let the IDE walk `A → B → C`
    /// cross-template breadcrumbs when a deep render spans three
    /// files. `None` for top-level spans.
    pub parent_span: Option<Box<PromptSourceSpan>>,
    /// Template URI for the file that authored this span. Top-level
    /// spans carry the root render's template uri; included-child
    /// spans carry the included file's uri so breadcrumb navigation
    /// can open the right file when the user clicks through the
    /// `parent_span` chain. Defaults to empty string for callers that
    /// don't plumb it through.
    pub template_uri: String,
}

/// One conditional or section decision recorded during a template
/// render. Powers the "variant resolution" trace surfaced in the
/// portal so on-call engineers can answer "which capability branch
/// fired for this model?" without re-running the template. Recorded
/// deterministically — same `llm` snapshot + bindings always produce
/// the same trace, which is what makes replay reproducible (#1668).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BranchDecision {
    pub kind: BranchKind,
    pub template_uri: String,
    pub line: usize,
    pub col: usize,
    /// Short identifier for the taken branch. For `{{ if }}`/`elif`:
    /// `"if"`, `"elif:<idx>"`, `"else"`, or `"none"` when nothing
    /// matched and no `{{ else }}` was provided. For `{{ section }}`:
    /// the materialized envelope (e.g. `"xml"`, `"markdown"`,
    /// `"native_tools"`, `"react"`).
    pub branch_id: String,
    /// Human-readable label. For conditionals: the source-derived
    /// condition expression (e.g. `llm.capabilities.native_tools`).
    /// For sections: the section name (e.g. `tools`).
    pub branch_label: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchKind {
    If,
    Section,
}

impl BranchKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BranchKind::If => "if",
            BranchKind::Section => "section",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptSpanKind {
    /// Literal template text between directives.
    Text,
    /// `{{ expr }}` interpolation — the most common kind the IDE
    /// wants to highlight on hover.
    Expr,
    /// Legacy bare `{{ident}}` fallthrough, surfaced separately so the
    /// IDE can visually distinguish resolved from pass-through.
    LegacyBareInterp,
    /// Conditional branch text that actually rendered (the taken branch).
    If,
    /// One loop iteration's rendered body.
    ForIteration,
    /// Rendered partial/include expansion. Child spans carry the
    /// included template's own `template_uri`.
    Include,
    /// Capability-adaptive logical prompt section.
    Section,
}

/// Provenance-aware rendering. Returns the rendered string plus — when
/// `collect_provenance` is true — one `PromptSourceSpan` per node so the
/// IDE can link rendered byte ranges back to template source offsets.
/// When `collect_provenance` is false, this degrades to the cheap
/// non-tracked rendering path that the legacy callers use.
pub(crate) fn render_template_with_provenance(
    template: &str,
    bindings: Option<&BTreeMap<String, VmValue>>,
    base: Option<&Path>,
    source_path: Option<&Path>,
    collect_provenance: bool,
) -> Result<(String, Vec<PromptSourceSpan>), TemplateError> {
    let asset = TemplateAsset::inline(template, base, source_path);
    render_asset_with_provenance_result(&asset, bindings, collect_provenance)
}

pub(crate) fn render_asset_result(
    asset: &TemplateAsset,
    bindings: Option<&BTreeMap<String, VmValue>>,
) -> Result<String, TemplateError> {
    let (rendered, _spans) = render_asset_with_provenance_result(asset, bindings, false)?;
    Ok(rendered)
}

pub(crate) fn render_stdlib_prompt_asset(
    path: &str,
    bindings: Option<&BTreeMap<String, VmValue>>,
) -> Result<String, VmError> {
    let target = if path.starts_with("std/") {
        path.to_string()
    } else {
        format!("std/{path}")
    };
    let asset = TemplateAsset::render_target(&target).map_err(VmError::Runtime)?;
    render_asset_result(&asset, bindings).map_err(VmError::from)
}

/// Test-only helper: render an inline template under the active LLM
/// render context and return the rendered text plus the branch trace
/// that drove `template.render` event emission. The same trace is
/// emitted to the transcript JSONL when a transcript dir is wired in;
/// exposing it here lets unit tests assert determinism without
/// scraping the JSONL.
#[cfg(test)]
pub(crate) fn render_template_collect_branch_trace(
    template: &str,
) -> Result<(String, Vec<BranchDecision>), TemplateError> {
    let asset = TemplateAsset::inline(template, None, None);
    render_asset_with_provenance_and_trace_result(&asset, None, false, true)
        .map(|(rendered, _spans, trace)| (rendered, trace))
}

pub(crate) fn render_asset_with_provenance_result(
    asset: &TemplateAsset,
    bindings: Option<&BTreeMap<String, VmValue>>,
    collect_provenance: bool,
) -> Result<(String, Vec<PromptSourceSpan>), TemplateError> {
    let (rendered, spans, _trace) =
        render_asset_with_provenance_and_trace_result(asset, bindings, collect_provenance, false)?;
    Ok((rendered, spans))
}

fn render_asset_with_provenance_and_trace_result(
    asset: &TemplateAsset,
    bindings: Option<&BTreeMap<String, VmValue>>,
    collect_provenance: bool,
    force_branch_trace: bool,
) -> Result<(String, Vec<PromptSourceSpan>, Vec<BranchDecision>), TemplateError> {
    let nodes = parse_cached(asset)?;
    let mut out = String::with_capacity(asset.source.len());
    // Materialize the ambient `llm` binding when the caller is inside
    // an LLM frame (`llm_call` / `agent_loop` / handler-stack). User
    // bindings that already supply `llm` win — emit a one-shot lint
    // warning to flag the shadowed auto-injection so the author can
    // rename their key.
    let augmented = augment_bindings_with_llm(asset, bindings);
    let scope_bindings = augmented.as_ref().or(bindings);
    let mut scope = Scope::new(scope_bindings);
    // Only collect a branch trace when an LLM frame is in scope —
    // that's the only context where the trace adds debugging value
    // (capability-adaptive rendering), and the empty-trace events
    // would otherwise spam every doc-gen / CI render.
    let llm_ctx = current_llm_render_context();
    let mut rc = RenderCtx {
        current_asset: asset.clone(),
        include_stack: Vec::new(),
        current_include_parent: None,
        branch_trace: (force_branch_trace || llm_ctx.is_some()).then(Vec::new),
    };
    let mut spans = if collect_provenance {
        Some(Vec::new())
    } else {
        None
    };
    render_nodes(&nodes, &mut scope, &mut rc, &mut out, spans.as_mut()).map_err(|mut e| {
        if e.path.is_none() {
            e.path = asset.error_path();
        }
        if e.uri.is_none() {
            e.uri = asset.error_uri();
        }
        e
    })?;
    let trace = rc.branch_trace.take().unwrap_or_default();
    if let Some(ctx) = llm_ctx {
        emit_template_render_event(asset, &ctx, &trace, out.len());
    }
    Ok((out, spans.unwrap_or_default(), trace))
}

/// Render a template and return the capability branch trace that drove
/// logical-section materialization. This is the deterministic counterpart
/// to the `template.render` transcript event and is used by prompt evals
/// that need to score section shape without scraping JSONL artifacts.
pub fn render_template_to_string_with_branch_trace(
    template: &str,
    bindings: Option<&BTreeMap<String, VmValue>>,
    base: Option<&Path>,
    source_path: Option<&Path>,
) -> Result<(String, Vec<BranchDecision>), String> {
    let asset = TemplateAsset::inline(template, base, source_path);
    render_asset_with_provenance_and_trace_result(&asset, bindings, false, true)
        .map(|(rendered, _spans, trace)| (rendered, trace))
        .map_err(|error| error.message())
}

/// Emit a `template.render` transcript event capturing the resolved
/// LLM identity + capability snapshot and the branch trace produced
/// during rendering. Implementation in
/// [`crate::llm::agent_observe::record_template_render`].
fn emit_template_render_event(
    asset: &TemplateAsset,
    ctx: &LlmRenderContext,
    trace: &[BranchDecision],
    rendered_bytes: usize,
) {
    crate::llm::agent_observe::record_template_render(
        &asset.uri,
        asset.template_revision_hash().as_str(),
        ctx,
        trace,
        rendered_bytes,
    );
}
