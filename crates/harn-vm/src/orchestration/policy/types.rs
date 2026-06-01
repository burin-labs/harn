//! Policy type definitions — shapes used to describe agent capability
//! ceilings, turn/model/transcript policies, and the per-tool argument
//! constraint machinery. Everything here is plain data (+ a single
//! helper, `enforce_tool_arg_constraints`, that operates on it).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::super::glob_match;
use super::{reject_tool, PolicyDenial};
use crate::agent_events::DenialGate;
use crate::tool_annotations::ToolAnnotations;
use crate::value::VmValue;

/// Extended policy that supports argument-level constraints.
///
/// `arg_key` names the argument whose string value must match one of
/// `arg_patterns`. It is the self-describing form and should be set
/// explicitly by the policy author. When absent, the enforcer falls
/// back to `tool_annotations[tool].arg_schema.path_params`. If neither is populated,
/// the constraint is skipped with a structured `log_warn` — the VM is
/// intentionally domain-agnostic and does not guess argument semantics
/// by name (no "path"/"file"/"command"/... fallback list).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ToolArgConstraint {
    /// Tool name to constrain (glob-matched against dispatched tool names).
    pub tool: String,
    /// Glob patterns that the resolved argument value must match.
    /// If empty, no argument constraint is applied.
    pub arg_patterns: Vec<String>,
    /// Optional argument key whose string value is the constraint target.
    /// When present, overrides any metadata-derived key.
    #[serde(default)]
    pub arg_key: Option<String>,
}

/// Check if a tool call satisfies argument constraints in the policy.
///
/// Resolution order for which argument value to match:
/// 1. `constraint.arg_key` if set (explicit, self-describing).
/// 2. `policy.tool_annotations[tool].arg_schema.path_params` (first key
///    that yields a string value in the args object).
/// 3. No candidate — `log_warn` and skip the constraint. The VM refuses
///    to guess which argument holds the domain-relevant value.
pub fn enforce_tool_arg_constraints(
    policy: &CapabilityPolicy,
    tool_name: &str,
    args: &serde_json::Value,
) -> Result<(), PolicyDenial> {
    for constraint in &policy.tool_arg_constraints {
        if !glob_match(&constraint.tool, tool_name) {
            continue;
        }
        if constraint.arg_patterns.is_empty() {
            continue;
        }

        // Never guess which arg the constraint targets by common names —
        // that's a pipeline-level concern, not a VM concern.
        let declared_keys: Vec<String> = if let Some(key) = constraint.arg_key.as_ref() {
            vec![key.clone()]
        } else {
            policy
                .tool_annotations
                .get(tool_name)
                .map(|a| a.arg_schema.path_params.clone())
                .unwrap_or_default()
        };

        let (arg_key, arg_value): (String, Option<String>) = if let Some(obj) = args.as_object() {
            if declared_keys.is_empty() {
                // Permissive by design: missing annotations warn instead of
                // blocking, so a misconfigured policy can't silently wedge work.
                crate::events::log_warn(
                    "policy.constraint_unresolved",
                    &format!(
                        "tool_arg_constraint for tool '{tool_name}' has no arg_key and tool_annotations.arg_schema.path_params is empty; skipping (policy author should declare arg_key on the constraint or path_params in the tool's annotations)"
                    ),
                );
                continue;
            }
            let mut found: (String, Option<String>) = (declared_keys[0].clone(), None);
            for param in &declared_keys {
                if let Some(value) = obj.get(param).and_then(|v| v.as_str()) {
                    found = (param.clone(), Some(value.to_string()));
                    break;
                }
            }
            found
        } else {
            ("value".to_string(), args.as_str().map(|s| s.to_string()))
        };

        // Absent arg ≠ rejection — constraint simply does not apply.
        let Some(candidate) = arg_value else {
            continue;
        };
        let matches = constraint
            .arg_patterns
            .iter()
            .any(|pattern| glob_match(pattern, &candidate));
        if !matches {
            return reject_tool(
                DenialGate::ArgConstraint,
                None,
                format!(
                    "tool '{tool_name}' {arg_key} '{candidate}' does not match allowed patterns: {:?}. \
                     Only the {arg_key} argument is checked against this allow-list — other argument \
                     values are not.",
                    constraint.arg_patterns
                ),
            );
        }
    }
    Ok(())
}

/// Selectable confinement strength for processes spawned under this
/// policy. Defaults to [`SandboxProfile::Worktree`] — workspace-roots
/// path enforcement plus best-effort OS confinement (warn-and-skip if
/// the platform mechanism is unavailable). Stricter callers opt into
/// [`SandboxProfile::OsHardened`], which requires the OS sandbox to
/// engage and surfaces a typed `tool_rejected` error otherwise.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SandboxProfile {
    /// No path enforcement, no OS confinement. Used by direct VM
    /// embeddings with no orchestration policy and by escape hatches
    /// that explicitly disable isolation.
    Unrestricted,
    /// Workspace-root path enforcement plus best-effort OS confinement.
    /// Honors `HARN_HANDLER_SANDBOX={off,warn,enforce}` for the OS
    /// portion. Default for `harn run` and orchestrator-launched
    /// workflows.
    #[default]
    Worktree,
    /// Workspace-root path enforcement plus required OS confinement.
    /// Spawns fail with `tool_rejected` if the platform's hardening
    /// mechanism (Linux Landlock+seccomp, macOS sandbox-exec, Windows
    /// AppContainer) is unavailable, regardless of
    /// `HARN_HANDLER_SANDBOX`.
    OsHardened,
    /// Testbench WASI sandbox — subprocess execution is replayed from
    /// recorded WASI modules instead of running on the host. Selected
    /// indirectly by `harn test bench --process-wasi <dir>`.
    Wasi,
}

impl SandboxProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            SandboxProfile::Unrestricted => "unrestricted",
            SandboxProfile::Worktree => "worktree",
            SandboxProfile::OsHardened => "os_hardened",
            SandboxProfile::Wasi => "wasi",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "unrestricted" => Some(SandboxProfile::Unrestricted),
            "worktree" => Some(SandboxProfile::Worktree),
            "os_hardened" => Some(SandboxProfile::OsHardened),
            "wasi" => Some(SandboxProfile::Wasi),
            _ => None,
        }
    }
}

/// Named host filesystem presets granted only to child-process OS
/// sandboxes. These do not widen Harn file builtins; they are used so
/// subprocesses can load runtimes, compilers, and cache files that live
/// outside the workspace while Harn's own read/write surface remains
/// scoped by `workspace_roots` and `read_only_roots`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum ProcessSandboxPreset {
    /// Minimal host runtime roots needed to execute common system binaries.
    SystemRuntime,
    /// OS/vendor developer toolchains such as Xcode, Command Line Tools,
    /// Homebrew, and language runtimes installed under standard system paths.
    DeveloperToolchains,
    /// Per-user scratch/cache locations used by developer tools. Write access
    /// is granted only when the active policy already allows workspace writes.
    UserTemp,
}

impl ProcessSandboxPreset {
    pub const fn default_presets() -> &'static [Self] {
        &[
            Self::SystemRuntime,
            Self::DeveloperToolchains,
            Self::UserTemp,
        ]
    }
}

/// Process-only filesystem policy layered onto the active sandbox profile.
///
/// `presets: None` means "use the runtime defaults"; `Some([])` is an
/// explicit request for no named presets. Extra roots are process-only:
/// they do not allow Harn file tools to read or write those paths.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ProcessSandboxPolicy {
    pub presets: Option<Vec<ProcessSandboxPreset>>,
    pub read_roots: Vec<String>,
    pub write_roots: Vec<String>,
}

impl ProcessSandboxPolicy {
    pub fn effective_presets(&self) -> Vec<ProcessSandboxPreset> {
        self.presets
            .clone()
            .unwrap_or_else(|| ProcessSandboxPreset::default_presets().to_vec())
    }

    pub fn extend(&mut self, other: &Self) {
        if let Some(presets) = other.presets.as_ref() {
            self.presets = Some(presets.clone());
        }
        extend_unique(&mut self.read_roots, &other.read_roots);
        extend_unique(&mut self.write_roots, &other.write_roots);
    }

    fn intersect(&self, requested: &Self) -> Self {
        let presets = match (&self.presets, &requested.presets) {
            (None, None) => None,
            _ => Some(intersect_presets(
                &self.effective_presets(),
                &requested.effective_presets(),
            )),
        };
        Self {
            presets,
            read_roots: intersect_roots(&self.read_roots, &requested.read_roots),
            write_roots: intersect_roots(&self.write_roots, &requested.write_roots),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CapabilityPolicy {
    pub tools: Vec<String>,
    pub capabilities: BTreeMap<String, Vec<String>>,
    pub workspace_roots: Vec<String>,
    /// Roots the workload may read but never write. A path resolving
    /// under one of these passes read scope checks yet is rejected for
    /// `write_text`/`delete`, and the generated OS sandbox profile
    /// grants it read-only. Intended to be disjoint from
    /// `workspace_roots` (which are read-write); cloud mounts lower
    /// their `FilesystemAccess::ReadOnly` entries here so a "read-only"
    /// mount is actually unwritable inside the sandbox.
    #[serde(default)]
    pub read_only_roots: Vec<String>,
    pub side_effect_level: Option<String>,
    /// Remaining Harn-side nested-execution depth. The
    /// `enter_nested_execution_policy` helper validates this at every
    /// `agent_loop`, `sub_agent_run`, `spawn_agent` worker, workflow
    /// stage, and nested-workflow surface: `Some(0)` rejects the
    /// launch with a categorized `BudgetExceeded` error; `Some(n>0)`
    /// allows the descent and gives the child `Some(n - 1)`. `None`
    /// disables the budget gate entirely.
    pub recursion_limit: Option<usize>,
    /// Argument-level constraints for specific tools.
    #[serde(default)]
    pub tool_arg_constraints: Vec<ToolArgConstraint>,
    /// Per-tool annotations (kind, arg schema, capabilities, side-effect
    /// level). Pipelines own the registry; the VM reads it.
    #[serde(default)]
    pub tool_annotations: BTreeMap<String, ToolAnnotations>,
    /// Confinement strength applied to subprocesses spawned under this
    /// policy. Defaults to [`SandboxProfile::Worktree`]; pipelines opt
    /// into [`SandboxProfile::OsHardened`] when the workload should
    /// refuse to run if the platform sandbox is unavailable.
    #[serde(default)]
    pub sandbox_profile: SandboxProfile,
    /// Process-only filesystem allowances layered into OS subprocess
    /// sandboxes without widening Harn file builtins.
    #[serde(default)]
    pub process_sandbox: ProcessSandboxPolicy,
}

impl CapabilityPolicy {
    pub fn intersect(&self, requested: &CapabilityPolicy) -> Result<CapabilityPolicy, String> {
        let side_effect_level = match (&self.side_effect_level, &requested.side_effect_level) {
            (Some(a), Some(b)) => Some(min_side_effect(a, b).to_string()),
            (Some(a), None) => Some(a.clone()),
            (None, Some(b)) => Some(b.clone()),
            (None, None) => None,
        };

        if !self.tools.is_empty() {
            let denied: Vec<String> = requested
                .tools
                .iter()
                .filter(|tool| !self.tools.contains(*tool))
                .cloned()
                .collect();
            if !denied.is_empty() {
                return Err(format!(
                    "requested tools exceed host ceiling: {}",
                    denied.join(", ")
                ));
            }
        }

        for (capability, requested_ops) in &requested.capabilities {
            if let Some(allowed_ops) = self.capabilities.get(capability) {
                let denied: Vec<String> = requested_ops
                    .iter()
                    .filter(|op| !allowed_ops.contains(*op))
                    .cloned()
                    .collect();
                if !denied.is_empty() {
                    return Err(format!(
                        "requested capability operations exceed host ceiling: {}.{}",
                        capability,
                        denied.join(",")
                    ));
                }
            } else if !self.capabilities.is_empty() {
                return Err(format!(
                    "requested capability exceeds host ceiling: {capability}"
                ));
            }
        }

        let tools = if self.tools.is_empty() {
            requested.tools.clone()
        } else if requested.tools.is_empty() {
            self.tools.clone()
        } else {
            requested
                .tools
                .iter()
                .filter(|tool| self.tools.contains(*tool))
                .cloned()
                .collect()
        };

        let capabilities = if self.capabilities.is_empty() {
            requested.capabilities.clone()
        } else if requested.capabilities.is_empty() {
            self.capabilities.clone()
        } else {
            requested
                .capabilities
                .iter()
                .filter_map(|(capability, requested_ops)| {
                    self.capabilities.get(capability).map(|allowed_ops| {
                        (
                            capability.clone(),
                            requested_ops
                                .iter()
                                .filter(|op| allowed_ops.contains(*op))
                                .cloned()
                                .collect::<Vec<_>>(),
                        )
                    })
                })
                .collect()
        };

        let workspace_roots = intersect_roots(&self.workspace_roots, &requested.workspace_roots);
        let read_only_roots = intersect_roots(&self.read_only_roots, &requested.read_only_roots);

        let recursion_limit = match (self.recursion_limit, requested.recursion_limit) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };

        let mut tool_arg_constraints = self.tool_arg_constraints.clone();
        tool_arg_constraints.extend(requested.tool_arg_constraints.clone());

        let tool_annotations = tools
            .iter()
            .filter_map(|tool| {
                requested
                    .tool_annotations
                    .get(tool)
                    .or_else(|| self.tool_annotations.get(tool))
                    .cloned()
                    .map(|annotations| (tool.clone(), annotations))
            })
            .collect();

        // The sandbox profile composes via "max strictness wins" so
        // intersecting a worktree ceiling with an os_hardened request
        // yields os_hardened (the host gets the stricter of the two).
        let sandbox_profile =
            strictest_sandbox_profile(self.sandbox_profile, requested.sandbox_profile);
        let process_sandbox = self.process_sandbox.intersect(&requested.process_sandbox);

        Ok(CapabilityPolicy {
            tools,
            capabilities,
            workspace_roots,
            read_only_roots,
            side_effect_level,
            recursion_limit,
            tool_arg_constraints,
            tool_annotations,
            sandbox_profile,
            process_sandbox,
        })
    }
}

/// Intersect two root allowlists with the same "empty means unbounded"
/// convention the rest of `intersect` uses: an empty list on either side
/// is treated as "no ceiling on this dimension", so the other side passes
/// through untouched. When both are populated the result keeps only the
/// roots present in both.
fn intersect_roots(host: &[String], requested: &[String]) -> Vec<String> {
    if host.is_empty() {
        requested.to_vec()
    } else if requested.is_empty() {
        host.to_vec()
    } else {
        requested
            .iter()
            .filter(|root| host.contains(*root))
            .cloned()
            .collect()
    }
}

fn sandbox_profile_strictness(profile: SandboxProfile) -> u8 {
    match profile {
        SandboxProfile::Unrestricted => 0,
        SandboxProfile::Worktree => 1,
        SandboxProfile::Wasi => 2,
        SandboxProfile::OsHardened => 3,
    }
}

fn strictest_sandbox_profile(left: SandboxProfile, right: SandboxProfile) -> SandboxProfile {
    if sandbox_profile_strictness(left) >= sandbox_profile_strictness(right) {
        left
    } else {
        right
    }
}

fn intersect_presets(
    left: &[ProcessSandboxPreset],
    right: &[ProcessSandboxPreset],
) -> Vec<ProcessSandboxPreset> {
    left.iter()
        .filter(|preset| right.contains(preset))
        .copied()
        .collect()
}

fn extend_unique(target: &mut Vec<String>, roots: &[String]) {
    for root in roots {
        if !target.contains(root) {
            target.push(root.clone());
        }
    }
}

fn min_side_effect<'a>(a: &'a str, b: &'a str) -> &'a str {
    fn rank(v: &str) -> usize {
        match v {
            "none" => 0,
            "read_only" => 1,
            "workspace_write" => 2,
            "process_exec" => 3,
            "network" => 4,
            _ => 5,
        }
    }
    if rank(a) <= rank(b) {
        a
    } else {
        b
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TurnPolicy {
    /// When true, text-only responses in a tool-capable stage are treated as
    /// invalid unless they switch phase / finish the stage. This keeps action
    /// stages moving instead of drifting into narration.
    pub require_action_or_yield: bool,
    /// When false, workflow-owned action stages should hand control back via
    /// successful tool calls instead of advertising an additional done
    /// sentinel pathway in corrective nudges.
    #[serde(default = "default_true")]
    pub allow_done_sentinel: bool,
    /// Optional visible prose budget for a single assistant turn. When the
    /// assistant exceeds it, the recorded transcript keeps only a shortened
    /// version and the next corrective nudge reminds the model to stay brief.
    pub max_prose_chars: Option<usize>,
}

impl Default for TurnPolicy {
    fn default() -> Self {
        Self {
            require_action_or_yield: false,
            allow_done_sentinel: true,
            max_prose_chars: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeToolFallbackPolicy {
    Allow,
    AllowOnce,
    #[default]
    Reject,
}

impl NativeToolFallbackPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::AllowOnce => "allow_once",
            Self::Reject => "reject",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "allow" => Some(Self::Allow),
            "allow_once" => Some(Self::AllowOnce),
            "reject" => Some(Self::Reject),
            _ => None,
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ModelPolicy {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub model_tier: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<i64>,
    /// Maximum agent_loop iterations for this stage. Overrides the default 16.
    /// Static cap; prefer `iteration_budget` so the per-stage `agent_loop`
    /// receives Harn's adaptive `loop_control` policy and emits
    /// `loop_control_decision` events on extension/stop. Both fields are
    /// passed through to `agent_loop`; if both are present, the budget's
    /// `max` wins because `agent_loop` reads `iteration_budget` first.
    pub max_iterations: Option<usize>,
    /// Adaptive iteration budget for this stage. Surfaced on the per-stage
    /// `agent_loop` so the runtime emits `loop_control_decision` events
    /// when extending or stopping the loop. Pipelines author this through
    /// `agent_preset(...) / agent_budget(...)` from `std/agent/presets`,
    /// or as a literal dict like `{mode: "adaptive", initial: 4, max: 16, extend_by: 2}`.
    /// Stored as a free-form JSON value so the std/agent budget shapes
    /// can evolve without churn here; `__normalize_iteration_budget` in
    /// `std/agent/options` does the per-call validation.
    pub iteration_budget: Option<serde_json::Value>,
    /// Maximum consecutive text-only (no tool call) responses before declaring stuck.
    pub max_nudges: Option<usize>,
    /// Custom nudge message injected when the model produces text without tool calls.
    /// If omitted, the VM uses a generic "Continue — use a tool call" message.
    pub nudge: Option<String>,
    /// Few-shot tool-call examples injected into the tool contract prompt,
    /// shown before the tool schema listing. Pipelines provide these —
    /// the VM has no hardcoded tool names.
    pub tool_examples: Option<String>,
    /// Optional Harn closure called after each tool-calling turn.
    /// Receives turn metadata; returns either a string user message to inject,
    /// a bool stop flag, or a dict like {message, stop}.
    /// Wrapped in EqIgnored so it doesn't affect PartialEq derivation.
    #[serde(skip)]
    pub post_turn_callback: Option<EqIgnored<VmValue>>,
    /// When set, the stage stops after any tool-calling turn whose successful
    /// results include one of these tool names. This is useful for
    /// workflow-owned verify loops where a productive write turn should hand
    /// control back to verification immediately.
    pub stop_after_successful_tools: Option<Vec<String>>,
    /// When set, the stage is reported as failed unless at least one of these
    /// tool names succeeds during the interaction. Pipelines use this to
    /// assert a stage cannot quietly finish without running a specific tool.
    pub require_successful_tools: Option<Vec<String>>,
    /// Turn-shape constraints for action stages.
    pub turn_policy: Option<TurnPolicy>,
    /// Tool-calling contract format for the per-stage agent loop.
    /// `"native"` routes user tools through the provider's native
    /// function-call channel; `"text"` embeds the `<tool_call>` contract
    /// prompt. Mirrors the top-level `agent_loop(..., {tool_format: ...})`
    /// option. When `None`, std/workflow/options.harn composes the fallback
    /// from host-provided env/default facts.
    pub tool_format: Option<String>,
    /// Policy for native-tool stages when a provider emits text-mode
    /// `<tool_call>` output instead of native tool calls.
    pub native_tool_fallback: NativeToolFallbackPolicy,
}

/// Wrapper that always compares equal, allowing non-Eq types in derived PartialEq structs.
#[derive(Clone, Debug, Default)]
pub struct EqIgnored<T>(pub T);

impl<T> PartialEq for EqIgnored<T> {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl<T> std::ops::Deref for EqIgnored<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

/// Per-call auto-compaction settings. First-class replacement for
/// what used to live on `TranscriptPolicy` — the mode/fork/compact
/// lifecycle fields are gone; lifecycle is driven by explicit
/// `agent_session_*` builtins. Only the per-call tuning for how the
/// agent loop manages its context window survives.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AutoCompactPolicy {
    /// Enable per-turn auto-compaction within agent loops.
    pub enabled: bool,
    /// Token threshold for tier-1 compaction.
    pub token_threshold: Option<usize>,
    /// Max chars per tool result before compression.
    pub tool_output_max_chars: Option<usize>,
    /// Tier-1 compaction strategy name (e.g., "observation_mask", "llm").
    pub compact_strategy: Option<String>,
    /// Token threshold for tier-2 aggressive compaction.
    pub hard_limit_tokens: Option<usize>,
    /// Tier-2 compaction strategy name.
    pub hard_limit_strategy: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextPolicy {
    pub max_artifacts: Option<usize>,
    pub max_tokens: Option<usize>,
    pub reserve_tokens: Option<usize>,
    pub include_kinds: Vec<String>,
    pub exclude_kinds: Vec<String>,
    pub prioritize_kinds: Vec<String>,
    pub pinned_ids: Vec<String>,
    pub include_stages: Vec<String>,
    pub prefer_recent: bool,
    pub prefer_fresh: bool,
    pub render: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RetryPolicy {
    pub max_attempts: usize,
    pub verify: bool,
    pub repair: bool,
    /// Initial backoff duration in milliseconds between retry attempts.
    /// When `None`, retries proceed without delay.
    #[serde(default)]
    pub backoff_ms: Option<u64>,
    /// Multiplier applied to `backoff_ms` after each retry attempt.
    /// Defaults to 2.0 when `backoff_ms` is set and this field is `None`.
    #[serde(default)]
    pub backoff_multiplier: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct StageContract {
    pub input_kinds: Vec<String>,
    pub output_kinds: Vec<String>,
    pub min_inputs: Option<usize>,
    pub max_inputs: Option<usize>,
    pub require_transcript: bool,
    pub schema: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BranchSemantics {
    pub success: Option<String>,
    pub failure: Option<String>,
    pub verify_pass: Option<String>,
    pub verify_fail: Option<String>,
    pub condition_true: Option<String>,
    pub condition_false: Option<String>,
    pub loop_continue: Option<String>,
    pub loop_exit: Option<String>,
    pub escalation: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct MapPolicy {
    pub items: Vec<serde_json::Value>,
    pub item_artifact_kind: Option<String>,
    pub output_kind: Option<String>,
    pub max_items: Option<usize>,
    pub max_concurrent: Option<usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct JoinPolicy {
    pub strategy: String,
    pub require_all_inputs: bool,
    pub min_completed: Option<usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReducePolicy {
    pub strategy: String,
    pub separator: Option<String>,
    pub output_kind: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EscalationPolicy {
    pub level: Option<String>,
    pub queue: Option<String>,
    pub reason: Option<String>,
}
