//! Long-lived agent-loop state. Extracted from `agent/mod.rs` so the
//! orchestrator can thread its state through phase methods instead of
//! juggling 40+ local bindings. The four RAII drop-guards that used to
//! live inline in `run_agent_loop_internal` now live here as owned
//! fields — their `Drop` impls still pop orchestration stacks and clear
//! session sinks on loop exit (success or error).

use std::rc::Rc;

use crate::llm::daemon::{watch_state, DaemonLoopConfig};
use crate::value::{VmError, VmValue};

use super::super::agent_config::AgentLoopConfig;
use super::super::agent_tools::{
    merge_agent_loop_approval_policy, merge_agent_loop_policy, normalize_native_tools_for_format,
    normalize_tool_choice_for_format, normalize_tool_examples_for_format, ToolCallTracker,
};
use super::super::tools::build_tool_calling_contract_prompt;

/// Resets the iteration counter in the transcript-observer thread-local
/// when the loop exits.
pub(super) struct TranscriptIterationGuard;

impl Drop for TranscriptIterationGuard {
    fn drop(&mut self) {
        crate::llm::agent_observe::set_current_iteration(None);
    }
}

/// Pops the loop-local execution policy off the orchestration stack
/// when the loop exits. Only pops if the loop actually pushed a policy.
pub(super) struct ExecutionPolicyGuard {
    pub(super) active: bool,
}

impl Drop for ExecutionPolicyGuard {
    fn drop(&mut self) {
        if self.active {
            crate::orchestration::pop_execution_policy();
        }
    }
}

/// Pops the loop-local approval policy off the orchestration stack when
/// the loop exits. Only pops if the loop actually pushed a policy.
pub(super) struct ApprovalPolicyGuard {
    pub(super) active: bool,
}

impl Drop for ApprovalPolicyGuard {
    fn drop(&mut self) {
        if self.active {
            crate::orchestration::pop_approval_policy();
        }
    }
}

/// Owned handle for a temporarily-reshaped `VmValue` tools registry.
/// The state struct doesn't own this — it holds a borrow into it via
/// `tools_val_borrow` only during construction. Dropped before `Self::new`
/// returns (the filtered value isn't needed once the contract prompt is
/// baked). The wrapper exists so the `Option<&VmValue>` borrow can live
/// alongside its backing `VmValue`.
struct VmValueOwned(crate::value::VmValue);

/// Build a `VmValue` equivalent to `tools_val` but with `defer_loading:
/// true` entries removed when they aren't already promoted. Returns
/// `None` when the input isn't a registry dict (nothing to filter) —
/// callers should fall back to the original `tools_val`.
fn filter_deferred_from_tools_val(
    tools_val: Option<&crate::value::VmValue>,
    client: &ClientToolSearchState,
) -> Option<crate::value::VmValue> {
    use crate::value::VmValue;
    use std::rc::Rc;

    let tools_val = tools_val?;
    let dict = tools_val.as_dict()?;
    let tools_list = match dict.get("tools") {
        Some(VmValue::List(list)) => list,
        _ => return None,
    };

    let mut kept: Vec<VmValue> = Vec::with_capacity(tools_list.len());
    for entry in tools_list.iter() {
        let is_hidden = match entry {
            VmValue::Dict(d) => {
                let is_deferred = matches!(d.get("defer_loading"), Some(VmValue::Bool(true)));
                if !is_deferred {
                    false
                } else {
                    let name = d.get("name").map(|v| v.display()).unwrap_or_default();
                    !(client.always_loaded.contains(&name) || client.promoted_set.contains(&name))
                }
            }
            _ => false,
        };
        if !is_hidden {
            kept.push(entry.clone());
        }
    }

    let mut new_dict = dict.clone();
    new_dict.insert("tools".to_string(), VmValue::List(Rc::new(kept)));
    Some(VmValue::Dict(Rc::new(new_dict)))
}

/// Client-mode tool_search state carried across turns. Present only
/// when `LlmCallOptions.tool_search` resolved to `ToolSearchMode::Client`
/// (explicitly or via auto-fallback from an unsupported provider).
///
/// The loop stashes the deferred tools' full native-shape JSON here so
/// it can re-add promoted tools to `opts.native_tools` *without*
/// re-walking the VM-side tool registry on every turn. Lookups are by
/// tool name.
pub(super) struct ClientToolSearchState {
    pub(super) synthetic_name: String,
    pub(super) strategy: crate::llm::api::ToolSearchStrategy,
    pub(super) variant: crate::llm::api::ToolSearchVariant,
    /// Tools the user pinned to the eager set. Consumed by the options
    /// layer's `apply_tool_search_client_injection`; kept here for
    /// diagnostics / future refresh logic (if we ever re-walk the
    /// registry on resume, the pin list is needed to seed the payload).
    #[allow(dead_code)]
    pub(super) always_loaded: std::collections::BTreeSet<String>,
    pub(super) budget_tokens: Option<i64>,
    /// Canonical copy of every deferred tool's native-shape JSON, keyed
    /// by tool name. Used to re-surface a tool when its name appears in
    /// a search result.
    pub(super) deferred_bodies: std::collections::BTreeMap<String, serde_json::Value>,
    /// Names currently promoted onto `opts.native_tools`. Kept in FIFO
    /// promotion order so oldest-eviction is O(1).
    pub(super) promoted_order: Vec<String>,
    pub(super) promoted_set: std::collections::BTreeSet<String>,
    /// Running sum of tokens attributable to currently-promoted tool
    /// schemas (approximate — one-quarter of the tool JSON's serialized
    /// char count). Used to enforce the `budget_tokens` soft cap without
    /// a second JSON walk per turn.
    pub(super) promoted_token_estimate: std::collections::BTreeMap<String, i64>,
}

impl ClientToolSearchState {
    /// Estimate the token cost of a tool's JSON schema. One token ≈ 4
    /// characters is the rule-of-thumb across major tokenizers
    /// (Claude, GPT-4, Llama). Close enough for budget accounting —
    /// overshooting by a few percent is fine; undershooting is the
    /// failure mode.
    pub(super) fn estimate_tokens(body: &serde_json::Value) -> i64 {
        let s = serde_json::to_string(body).unwrap_or_default();
        ((s.len() as f64) / 4.0).ceil() as i64
    }

    pub(super) fn current_token_total(&self) -> i64 {
        self.promoted_token_estimate.values().copied().sum()
    }
}

/// Drops every external sink and closure subscriber registered against
/// this loop's session_id when the loop exits (success or error).
/// Without this, pipeline `agent_subscribe` closures would accumulate
/// across workflow stages — each stage builder calls `agent_subscribe`
/// exactly once for its own session_id, but the registry only cleared
/// when the ACP server explicitly tore a session down. CLI / non-ACP
/// embeddings leaked monotonically.
pub(super) struct SessionSinkGuard {
    pub(super) session_id: String,
}

impl Drop for SessionSinkGuard {
    fn drop(&mut self) {
        if !self.session_id.is_empty() {
            crate::agent_events::clear_session_sinks(&self.session_id);
        }
    }
}

/// Strategy for picking a skill from the registry. `Metadata` runs
/// entirely inside the VM; `Host` / `Embedding` delegate to the host via
/// the `skill/match` bridge RPC.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SkillMatchStrategy {
    #[default]
    Metadata,
    Host,
    Embedding,
}

impl SkillMatchStrategy {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "host" => Self::Host,
            "embedding" => Self::Embedding,
            "metadata" | "" => Self::Metadata,
            other => {
                crate::events::log_warn(
                    "agent.skill_match",
                    &format!("unknown strategy '{other}', falling back to 'metadata'"),
                );
                Self::Metadata
            }
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Host => "host",
            Self::Embedding => "embedding",
        }
    }
}

/// User-facing `skill_match:` config dict parsed off the agent_loop
/// options dict.
#[derive(Clone, Debug)]
pub struct SkillMatchConfig {
    pub strategy: SkillMatchStrategy,
    pub top_n: usize,
    /// When `true`, once a skill activates it stays active for the rest
    /// of the loop. When `false`, the reassess phase re-runs after each
    /// turn and may swap it out.
    pub sticky: bool,
}

impl Default for SkillMatchConfig {
    fn default() -> Self {
        Self {
            strategy: SkillMatchStrategy::Metadata,
            top_n: 1,
            sticky: true,
        }
    }
}

/// Concrete per-skill metadata after activation. Derived from the raw
/// skill-registry entry once on activation so downstream phases don't
/// re-walk VM dicts every turn.
#[derive(Clone, Debug, Default)]
#[allow(dead_code)]
pub struct ActiveSkill {
    pub name: String,
    pub description: String,
    pub prompt: Option<String>,
    pub when_to_use: String,
    pub paths: Vec<String>,
    pub allowed_tools: Vec<String>,
    pub mcp_servers: Vec<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub invocation: String,
    pub disable_model_invocation: bool,
    pub user_invocable: bool,
}

impl ActiveSkill {
    /// Project a raw skill-registry entry (a `VmValue::Dict`) into the
    /// strongly-typed activation view. Tolerates missing fields with
    /// defaults matching Anthropic's SKILL.md frontmatter semantics
    /// (`user-invocable` defaults to `true`, `disable-model-invocation`
    /// to `false`).
    ///
    /// Single source of truth for ActiveSkill construction — the
    /// matching phase and the session-rehydration path both route
    /// through here so the two can't drift.
    pub(crate) fn from_entry(entry: &VmValue) -> Self {
        let Some(dict) = entry.as_dict() else {
            return Self::default();
        };
        let list_strings = |v: Option<&VmValue>| -> Vec<String> {
            match v {
                Some(VmValue::List(list)) => list.iter().map(|x| x.display()).collect(),
                _ => Vec::new(),
            }
        };
        let non_empty = |v: Option<&VmValue>| -> Option<String> {
            v.map(|value| value.display()).filter(|s| !s.is_empty())
        };
        let bool_with = |keys: &[&str], default: bool| -> bool {
            keys.iter()
                .find_map(|key| dict.get(*key))
                .map(|v| matches!(v, VmValue::Bool(true)))
                .unwrap_or(default)
        };
        Self {
            name: dict.get("name").map(|v| v.display()).unwrap_or_default(),
            description: dict
                .get("description")
                .map(|v| v.display())
                .unwrap_or_default(),
            prompt: non_empty(dict.get("prompt")),
            when_to_use: dict
                .get("when_to_use")
                .map(|v| v.display())
                .unwrap_or_default(),
            paths: list_strings(dict.get("paths")),
            allowed_tools: list_strings(dict.get("allowed_tools")),
            // Accept both `mcp` (legacy) and `requires_mcp` (harn#75
            // canonical). If both are present, union them so no user
            // ever loses a declared binding to a naming inconsistency.
            mcp_servers: {
                let mut servers = list_strings(dict.get("mcp"));
                for extra in list_strings(dict.get("requires_mcp")) {
                    if !servers.contains(&extra) {
                        servers.push(extra);
                    }
                }
                servers
            },
            model: non_empty(dict.get("model")),
            effort: non_empty(dict.get("effort")),
            invocation: dict
                .get("invocation")
                .map(|v| v.display())
                .unwrap_or_default(),
            disable_model_invocation: bool_with(
                &["disable-model-invocation", "disable_model_invocation"],
                false,
            ),
            user_invocable: bool_with(&["user-invocable", "user_invocable"], true),
        }
    }

    /// True when the skill's `disable-model-invocation` frontmatter is
    /// set. Used by the metadata matcher to filter candidates.
    pub(crate) fn is_disabled_for_model(entry: &VmValue) -> bool {
        let Some(dict) = entry.as_dict() else {
            return false;
        };
        matches!(
            dict.get("disable-model-invocation")
                .or_else(|| dict.get("disable_model_invocation")),
            Some(VmValue::Bool(true))
        )
    }
}

/// Restore previously-active skills for a session when the caller
/// re-enters it. Anonymous (caller-less session_id) loops always start
/// empty — their transcript doesn't persist either.
///
/// Rehydration runs against the *current* skill registry, so a skill
/// that was active on the last run but has since been removed just
/// quietly drops off the active set — it won't block the match phase
/// from re-running.
fn rehydrate_active_skills(
    anonymous: bool,
    session_id: &str,
    registry: Option<&crate::value::VmValue>,
) -> Vec<ActiveSkill> {
    if anonymous {
        return Vec::new();
    }
    let Some(registry) = registry else {
        return Vec::new();
    };
    let Some(dict) = registry.as_dict() else {
        return Vec::new();
    };
    let skills: Vec<crate::value::VmValue> = match dict.get("skills") {
        Some(crate::value::VmValue::List(list)) => list.iter().cloned().collect(),
        _ => Vec::new(),
    };
    let names = crate::agent_sessions::active_skills(session_id);
    if names.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for name in names {
        let Some(entry) = skills.iter().find(|s| {
            s.as_dict()
                .and_then(|d| d.get("name"))
                .map(|v| v.display() == name)
                .unwrap_or(false)
        }) else {
            continue;
        };
        out.push(ActiveSkill::from_entry(entry));
    }
    out
}

/// Every long-lived local the agent loop carried in
/// `run_agent_loop_internal`, gathered onto a single owned struct so
/// phase methods can take `&mut self` and mutate state in place.
///
/// # Drop ordering
///
/// Rust drops struct fields in FORWARD declaration order. The original
/// function declared its drop guards via `let _x = …` bindings in this
/// sequence:
///
/// ```text
/// let _iteration_guard = …;  // 1st
/// let _sink_guard      = …;  // 2nd
/// let _policy_guard    = …;  // 3rd
/// let _approval_guard  = …;  // 4th
/// ```
///
/// `let` bindings drop in REVERSE declaration order, so the observable
/// drop order was: approval → policy → sink → iteration. To preserve
/// that order with struct fields (forward drop order), the guard fields
/// appear in this struct in the SAME sequence as the observable drop
/// order: `_approval_guard`, `_policy_guard`, `_sink_guard`,
/// `_iteration_guard`. **Do not reorder** without re-auditing the push/
/// pop lifetimes in `crate::orchestration`.
pub(super) struct AgentLoopState {
    pub(super) config: AgentLoopConfig,
    pub(super) session_id: String,
    /// True when the loop minted its own session id because no caller
    /// passed one in. Anonymous sessions are not persisted in the
    /// session store — the transcript lives only in this loop's state.
    pub(super) anonymous_session: bool,

    pub(super) tool_contract_prompt: Option<String>,
    pub(super) base_system: Option<String>,
    pub(super) persistent_system_prompt: Option<String>,
    pub(super) has_tools: bool,

    pub(super) visible_messages: Vec<serde_json::Value>,
    pub(super) recorded_messages: Vec<serde_json::Value>,
    pub(super) transcript_events: Vec<crate::value::VmValue>,
    pub(super) transcript_summary: Option<String>,
    pub(super) total_text: String,
    pub(super) last_iteration_text: String,

    pub(super) task_ledger: crate::llm::ledger::TaskLedger,
    pub(super) ledger_done_rejections: usize,
    pub(super) loop_tracker: ToolCallTracker,
    pub(super) loop_detect_enabled: bool,

    pub(super) total_iterations: usize,
    pub(super) resumed_iterations: usize,
    pub(super) consecutive_text_only: usize,
    pub(super) consecutive_single_tool_turns: usize,
    pub(super) idle_backoff_ms: u64,
    pub(super) last_run_exit_code: Option<i32>,

    pub(super) all_tools_used: Vec<String>,
    pub(super) successful_tools_used: Vec<String>,
    pub(super) rejected_tools: Vec<String>,
    pub(super) deferred_user_messages: Vec<String>,

    pub(super) daemon_state: String,
    pub(super) daemon_snapshot_path: Option<String>,
    pub(super) daemon_watch_state: std::collections::BTreeMap<String, u64>,

    /// Set only by the finalize phase.
    pub(super) final_status: &'static str,

    pub(super) loop_start: std::time::Instant,

    pub(super) bridge: Option<Rc<crate::bridge::HostBridge>>,
    pub(super) tool_format: String,
    pub(super) done_sentinel: String,
    pub(super) break_unless_phase: Option<String>,
    pub(super) max_iterations: usize,
    pub(super) max_nudges: usize,
    pub(super) tool_retries: usize,
    pub(super) tool_backoff_ms: u64,
    pub(super) exit_when_verified: bool,
    pub(super) persistent: bool,
    pub(super) daemon: bool,
    pub(super) auto_compact: Option<crate::orchestration::AutoCompactConfig>,
    pub(super) daemon_config: DaemonLoopConfig,
    pub(super) custom_nudge: Option<String>,

    /// Client-mode tool_search state — `None` unless
    /// `ToolSearchMode::Client` resolved for this loop (harn#70).
    pub(super) tool_search_client: Option<ClientToolSearchState>,

    /// Skill registry (validated `{_type: "skill_registry", skills: [...]}`)
    /// passed in via `agent_loop(…, {skills: …})`. `None` when no
    /// skill system is configured for this loop.
    pub(crate) skill_registry: Option<VmValue>,
    /// Match configuration — strategy, top_n, sticky flag.
    pub(crate) skill_match: SkillMatchConfig,
    /// Host-supplied working file set. Feeds `paths:` auto-trigger
    /// scoring in the metadata matcher and rides along as a hint to
    /// host-based matchers.
    pub(crate) working_files: Vec<String>,
    /// Currently-active skills after the most recent match / reassess
    /// phase. Empty when no skill activated. Preserves insertion order
    /// so deterministic top_n selection stays stable across turns.
    pub(crate) active_skills: Vec<ActiveSkill>,
    /// Skills loaded explicitly at runtime via `load_skill`. Kept
    /// separate from match-driven activation so reassess can continue
    /// updating `active_skills` without discarding deliberate
    /// progressive-disclosure loads.
    pub(crate) loaded_skills: Vec<ActiveSkill>,
    /// Snapshot of `opts.native_tools` taken in `new()` before any
    /// skill-scope narrowing. Used to restore the full tool list when
    /// an active skill deactivates. `None` when no native tools were
    /// configured.
    pub(super) native_tools_snapshot: Option<Vec<serde_json::Value>>,
    /// True when `active_skills` was populated from a persisted
    /// session. Signals the matching phase to preserve the restored
    /// set on iteration 0 instead of re-matching from scratch — that
    /// keeps session-resume semantics stable across agent_loop
    /// invocations under the same `session_id`.
    pub(crate) rehydrated_from_session: bool,

    // Drop guards: see "Drop ordering" on the struct docs.
    pub(super) _approval_guard: ApprovalPolicyGuard,
    pub(super) _policy_guard: ExecutionPolicyGuard,
    pub(super) _sink_guard: SessionSinkGuard,
    pub(super) _iteration_guard: TranscriptIterationGuard,
}

impl AgentLoopState {
    /// Union of `allowed_tools` across every active skill. Empty when
    /// no skill narrows the tool surface — callers should fall through
    /// to the unfiltered registry and native_tools list.
    pub(crate) fn skill_allowed_tools(&self) -> std::collections::BTreeSet<String> {
        self.active_skills
            .iter()
            .chain(self.loaded_skills.iter())
            .filter(|s| !s.allowed_tools.is_empty())
            .flat_map(|s| s.allowed_tools.iter().cloned())
            .collect()
    }

    /// Effective skill list for prompts: match-driven skills first,
    /// then any runtime-loaded skills that are not already active
    /// under the same name. When both exist, prefer the runtime-loaded
    /// version because it carries the substituted SKILL.md body.
    pub(crate) fn prompt_active_skills(&self) -> Vec<ActiveSkill> {
        let mut merged: Vec<ActiveSkill> = self.active_skills.clone();
        for loaded in &self.loaded_skills {
            if let Some(existing) = merged.iter_mut().find(|skill| skill.name == loaded.name) {
                *existing = loaded.clone();
            } else {
                merged.push(loaded.clone());
            }
        }
        merged
    }

    /// Produce a skill-scoped view of the original `tools_val` registry.
    /// When any active skill carries a non-empty `allowed_tools` list,
    /// the union of those names is the whitelist for the upcoming turn.
    /// Tools outside the whitelist are filtered out of the registry so
    /// the provider schema, contract prompt, and dispatch all see the
    /// narrower surface.
    ///
    /// Returns `None` when there's no active scope to apply — callers
    /// should fall through to the original registry.
    pub(crate) fn skill_scoped_tools_val(
        &self,
        tools_val: Option<&crate::value::VmValue>,
    ) -> Option<crate::value::VmValue> {
        use crate::value::VmValue;
        use std::rc::Rc;

        let tools_val = tools_val?;
        let dict = tools_val.as_dict()?;
        let allowed = self.skill_allowed_tools();
        if allowed.is_empty() {
            return None;
        }
        let tools_list = match dict.get("tools") {
            Some(VmValue::List(list)) => list,
            _ => return None,
        };
        let mut kept: Vec<VmValue> = Vec::with_capacity(tools_list.len());
        for entry in tools_list.iter() {
            let keep = match entry {
                VmValue::Dict(d) => d
                    .get("name")
                    .map(|v| v.display())
                    .map(|name| allowed.contains(&name))
                    .unwrap_or(false),
                _ => false,
            };
            if keep {
                kept.push(entry.clone());
            }
        }
        let mut new_dict = dict.clone();
        new_dict.insert("tools".to_string(), VmValue::List(Rc::new(kept)));
        Some(VmValue::Dict(Rc::new(new_dict)))
    }

    /// Rebuild `opts.native_tools` from the canonical snapshot, then
    /// apply the skill-scope whitelist. Single source of truth for
    /// "what the provider sees this turn": activate narrows, deactivate
    /// restores, and the rebuild is idempotent.
    ///
    /// No-op when `native_tools_snapshot` is `None` (text-mode contract
    /// prompt without a native channel).
    pub(super) fn rebuild_scoped_native_tools(&self, opts: &mut crate::llm::api::LlmCallOptions) {
        let Some(snapshot) = self.native_tools_snapshot.as_ref() else {
            return;
        };
        let allowed = self.skill_allowed_tools();
        if allowed.is_empty() {
            opts.native_tools = Some(snapshot.clone());
            return;
        }
        let filtered: Vec<serde_json::Value> = snapshot
            .iter()
            .filter(|entry| {
                let name = entry
                    .get("name")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        entry
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                    })
                    .unwrap_or("");
                // Always keep the synthetic `__harn_tool_search` tool —
                // a skill's `allowed_tools` gates the user-declared
                // surface, not the runtime's own scaffolding. Without
                // this, activating a skill while `tool_search` is
                // configured silently kills progressive disclosure.
                name.starts_with("__harn_") || name == "load_skill" || allowed.contains(name)
            })
            .cloned()
            .collect();
        opts.native_tools = Some(filtered);
    }

    /// Rebuild the tool-calling contract prompt for a turn with the
    /// current promoted-tool set factored in. Returns `None` when the
    /// loop isn't running in client-mode tool_search (use the baked-in
    /// `tool_contract_prompt` snapshot instead).
    ///
    /// Called once per turn from `turn_preflight` so that, after a
    /// `__harn_tool_search` call promotes `deploy_service` into
    /// `opts.native_tools`, the model sees its full schema on the
    /// next turn — not the stale turn-1 snapshot.
    pub(super) fn rebuild_tool_contract_prompt(
        &self,
        opts: &crate::llm::api::LlmCallOptions,
    ) -> Option<String> {
        let client = self.tool_search_client.as_ref()?;
        if !self.has_tools {
            return None;
        }
        let filtered = filter_deferred_from_tools_val(self.config_tools_val(opts).as_ref(), client);
        let tools_owned;
        let tools_val_borrow: Option<&crate::value::VmValue> = match (filtered, opts.tools.as_ref())
        {
            (Some(v), _) => {
                tools_owned = Some(v);
                tools_owned.as_ref()
            }
            (None, opt) => opt,
        };
        let native_tools_for_prompt = self.rebuild_native_tools_for_prompt(opts);
        let mut prompt = crate::llm::tools::build_tool_calling_contract_prompt(
            tools_val_borrow,
            native_tools_for_prompt.as_deref(),
            &self.tool_format,
            self.config
                .turn_policy
                .as_ref()
                .is_some_and(|policy| policy.require_action_or_yield),
            self.config.tool_examples.as_deref(),
        );
        if let Some(client_cfg) = opts.tool_search.as_ref().filter(|c| c.include_stub_listing) {
            let mut stub_lines = Vec::new();
            for (name, body) in &client_cfg.deferred_bodies {
                if client.promoted_set.contains(name) || client.always_loaded.contains(name) {
                    continue; // already surfaced; don't advertise as deferred
                }
                let description = body
                    .get("description")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        body.get("function")
                            .and_then(|f| f.get("description"))
                            .and_then(|v| v.as_str())
                    })
                    .unwrap_or("")
                    .split(['\n', '.'])
                    .next()
                    .unwrap_or("")
                    .trim();
                if description.is_empty() {
                    stub_lines.push(format!("- `{name}`"));
                } else {
                    stub_lines.push(format!("- `{name}` — {description}"));
                }
            }
            if !stub_lines.is_empty() {
                prompt.push_str(&format!(
                    "\n\n## Tools available via `{search_name}` (deferred)\n\n\
                     Call `{search_name}` with a query to surface any of:\n\n{list}\n",
                    search_name = client.synthetic_name,
                    list = stub_lines.join("\n"),
                ));
            }
        }
        Some(prompt)
    }

    /// The registry `VmValue` the user passed in, cloned for read-only
    /// access in helper methods that can't re-borrow from `opts.tools`
    /// through the mutable ref they hold.
    fn config_tools_val(
        &self,
        opts: &crate::llm::api::LlmCallOptions,
    ) -> Option<crate::value::VmValue> {
        opts.tools.clone()
    }

    /// Assemble the native-tools list used for the text-mode contract
    /// prompt on the current turn. Merges: the eager + synthetic tools
    /// currently in `opts.native_tools` (if any — empty in text mode)
    /// with the deferred bodies currently promoted. `build_tool_contract_prompt`
    /// dedups by name, so overlap is safe.
    fn rebuild_native_tools_for_prompt(
        &self,
        opts: &crate::llm::api::LlmCallOptions,
    ) -> Option<Vec<serde_json::Value>> {
        let client = self.tool_search_client.as_ref()?;
        let mut merged: Vec<serde_json::Value> = Vec::new();
        // Start with whatever the agent loop is currently shipping
        // (includes the synthetic tool + always-loaded + non-deferred).
        // In text mode `opts.native_tools` is None post-normalize —
        // fall back to a freshly-built list from the filtered tools_val.
        if let Some(native) = opts.native_tools.as_ref() {
            merged.extend(native.iter().cloned());
        } else if let Some(cfg) = opts.tool_search.as_ref() {
            // Text mode: reconstruct the synthetic tool + always-loaded
            // scaffold from the config so the prompt has something to
            // describe.
            merged.push(crate::llm::tools::build_client_search_tool_schema(
                &opts.provider,
                cfg,
            ));
        }
        // Add promoted deferred bodies that aren't already there.
        let seen: std::collections::BTreeSet<String> = merged
            .iter()
            .filter_map(|t| {
                t.get("name")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        t.get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                    })
                    .map(String::from)
            })
            .collect();
        for name in &client.promoted_order {
            if seen.contains(name) {
                continue;
            }
            if let Some(body) = client.deferred_bodies.get(name) {
                merged.push(body.clone());
            }
        }
        Some(merged)
    }
    /// Build the loop state from a fresh `AgentLoopConfig`, mutating
    /// `opts` in place to normalize the native-tool channel before the
    /// first LLM call.
    pub(super) fn new(
        opts: &mut crate::llm::api::LlmCallOptions,
        config: AgentLoopConfig,
    ) -> Result<Self, VmError> {
        // Fresh transcript segment per top-level loop: reset dedup so the
        // first call re-emits system_prompt + tool_schemas and message
        // events carry meaningful iteration indices.
        crate::llm::agent_observe::reset_transcript_dedup();

        let _iteration_guard = TranscriptIterationGuard;

        let bridge = super::current_host_bridge();
        let max_iterations = config.max_iterations;
        let persistent = config.persistent;
        let max_nudges = config.max_nudges;
        let config_skill_registry = config.skill_registry.clone();
        let config_skill_match = config.skill_match.clone();
        let config_working_files = config.working_files.clone();
        let custom_nudge = config.nudge.clone();
        let done_sentinel = config
            .done_sentinel
            .clone()
            .unwrap_or_else(|| "##DONE##".to_string());
        let break_unless_phase = config.break_unless_phase.clone();
        let tool_retries = config.tool_retries;
        let tool_backoff_ms = config.tool_backoff_ms;
        let tool_format = config.tool_format.clone();
        let (session_id, anonymous_session) = if config.session_id.trim().is_empty() {
            (format!("agent_session_{}", uuid::Uuid::now_v7()), true)
        } else {
            let resolved = crate::agent_sessions::open_or_create(Some(config.session_id.clone()));
            (resolved, false)
        };
        if !anonymous_session {
            let prior = crate::agent_sessions::messages_json(&session_id);
            if !prior.is_empty() {
                let caller_msgs = std::mem::take(&mut opts.messages);
                opts.messages = prior;
                opts.messages.extend(caller_msgs);
            }
        }
        let _sink_guard = SessionSinkGuard {
            session_id: session_id.clone(),
        };

        let auto_compact = config.auto_compact.clone();
        let daemon = config.daemon;
        let daemon_config = config.daemon_config.clone();
        let exit_when_verified = config.exit_when_verified;
        let last_run_exit_code: Option<i32> = None;

        let loop_detect_enabled = config.loop_detect_warn > 0;
        let loop_tracker = ToolCallTracker::new(
            config.loop_detect_warn,
            config.loop_detect_block,
            config.loop_detect_skip,
        );

        let effective_policy = merge_agent_loop_policy(config.policy.clone())?;

        // Intersect with outer workflow/worker ceiling so nested loops
        // never widen permissions.
        if let Some(ref policy) = effective_policy {
            crate::orchestration::push_execution_policy(policy.clone());
        }
        let _policy_guard = ExecutionPolicyGuard {
            active: effective_policy.is_some(),
        };

        let effective_approval_policy =
            merge_agent_loop_approval_policy(config.approval_policy.clone());
        if let Some(ref policy) = effective_approval_policy {
            crate::orchestration::push_approval_policy(policy.clone());
        }
        let _approval_guard = ApprovalPolicyGuard {
            active: effective_approval_policy.is_some(),
        };

        let has_skill_registry = config_skill_registry
            .as_ref()
            .and_then(|value| value.as_dict())
            .and_then(|dict| dict.get("skills"))
            .and_then(|value| match value {
                crate::value::VmValue::List(skills) => Some(skills),
                _ => None,
            })
            .is_some_and(|skills| !skills.is_empty());
        if has_skill_registry {
            let schema = crate::llm::tools::build_load_skill_tool_schema(&opts.provider);
            match opts.native_tools.as_mut() {
                Some(native_tools) => {
                    let already_present = native_tools.iter().any(|tool| {
                        tool.get("name").and_then(|v| v.as_str()).or_else(|| {
                            tool.get("function")
                                .and_then(|function| function.get("name"))
                                .and_then(|v| v.as_str())
                        }) == Some("load_skill")
                    });
                    if !already_present {
                        native_tools.insert(0, schema);
                    }
                }
                None => opts.native_tools = Some(vec![schema]),
            }
        }

        let tools_owned = opts.tools.clone();
        let tools_val = tools_owned.as_ref();
        // Capture client-mode tool_search state BEFORE
        // `normalize_native_tools_for_format` touches the list —
        // normalization preserves the synthetic tool and strips nothing
        // deferred (deferred tools were already set aside by option
        // parsing), but pulling the bodies here keeps the state
        // construction simple.
        let tool_search_client = opts
            .tool_search
            .as_ref()
            .filter(|cfg| {
                cfg.mode == crate::llm::api::ToolSearchMode::Client
                    || (cfg.mode == crate::llm::api::ToolSearchMode::Auto
                        && !crate::llm::provider::provider_supports_defer_loading(
                            &opts.provider,
                            &opts.model,
                        ))
            })
            .map(|cfg| ClientToolSearchState {
                synthetic_name: cfg.effective_name().to_string(),
                strategy: cfg.effective_strategy(),
                variant: cfg.variant,
                always_loaded: cfg.always_loaded.iter().cloned().collect(),
                budget_tokens: cfg.budget_tokens,
                deferred_bodies: cfg.deferred_bodies.clone(),
                promoted_order: Vec::new(),
                promoted_set: std::collections::BTreeSet::new(),
                promoted_token_estimate: std::collections::BTreeMap::new(),
            });
        // Snapshot native_tools *before* format-normalization so the
        // text-mode contract prompt can still render the synthetic
        // `__harn_tool_search` tool (normalization drops native_tools
        // entirely for non-native formats — fine for payload, lossy
        // for the prompt).
        let native_tools_for_prompt = opts.native_tools.clone();
        opts.native_tools =
            normalize_native_tools_for_format(&tool_format, opts.native_tools.clone());
        // Capture the post-normalization native_tools list as the
        // canonical full set. Subsequent turn_preflights use this to
        // rebuild `opts.native_tools` from scratch — activate narrows,
        // deactivate restores the full surface.
        let native_tools_snapshot = opts.native_tools.clone();
        opts.tool_choice = normalize_tool_choice_for_format(
            &opts.provider,
            &tool_format,
            opts.native_tools.as_deref(),
            opts.tool_choice.clone(),
            config.turn_policy.as_ref(),
        );
        // When client-mode tool_search is active, build a filtered
        // version of tools_val that hides deferred tools from the
        // contract prompt — they're surfaced lazily via the synthetic
        // search tool. Without this, `collect_vm_tool_schemas` would
        // still emit every declared tool's schema, defeating the token
        // savings that are the whole point of progressive disclosure.
        let tools_val_for_prompt = tool_search_client
            .as_ref()
            .and_then(|client| filter_deferred_from_tools_val(tools_val, client))
            .map(VmValueOwned);
        let tools_val_borrow = tools_val_for_prompt.as_ref().map(|v| &v.0).or(tools_val);
        let rendered_schemas = crate::llm::tools::collect_tool_schemas(
            tools_val_borrow,
            native_tools_for_prompt.as_deref(),
        );
        let has_tools = !rendered_schemas.is_empty();
        let base_system = opts.system.clone();
        let tool_examples =
            normalize_tool_examples_for_format(&tool_format, config.tool_examples.clone());
        let tool_contract_prompt = if has_tools {
            let mut prompt = build_tool_calling_contract_prompt(
                tools_val_borrow,
                native_tools_for_prompt.as_deref(),
                &tool_format,
                config
                    .turn_policy
                    .as_ref()
                    .is_some_and(|policy| policy.require_action_or_yield),
                tool_examples.as_deref(),
            );
            // Client-mode tool_search: when the user opted into stub
            // listings, append a short "also available via search"
            // paragraph so the model knows what's searchable without
            // calling the search tool first. Matches Anthropic's
            // described ergonomic even though the native path doesn't
            // need it (the server handles stubs there).
            if let Some(client_cfg) = opts.tool_search.as_ref().filter(|c| {
                c.include_stub_listing
                    && (c.mode == crate::llm::api::ToolSearchMode::Client
                        || (c.mode == crate::llm::api::ToolSearchMode::Auto
                            && !crate::llm::provider::provider_supports_defer_loading(
                                &opts.provider,
                                &opts.model,
                            )))
            }) {
                let mut stub_lines = Vec::new();
                for (name, body) in &client_cfg.deferred_bodies {
                    let description = body
                        .get("description")
                        .and_then(|v| v.as_str())
                        .or_else(|| {
                            body.get("function")
                                .and_then(|f| f.get("description"))
                                .and_then(|v| v.as_str())
                        })
                        .unwrap_or("")
                        .split(['\n', '.'])
                        .next()
                        .unwrap_or("")
                        .trim();
                    if description.is_empty() {
                        stub_lines.push(format!("- `{name}`"));
                    } else {
                        stub_lines.push(format!("- `{name}` — {description}"));
                    }
                }
                if !stub_lines.is_empty() {
                    prompt.push_str(&format!(
                        "\n\n## Tools available via `{search_name}` (deferred)\n\n\
                         Call `{search_name}` with a query to surface any of:\n\n{list}\n",
                        search_name = client_cfg.effective_name(),
                        list = stub_lines.join("\n"),
                    ));
                }
            }
            Some(prompt)
        } else {
            None
        };

        let allow_done_sentinel = config
            .turn_policy
            .as_ref()
            .map(|policy| policy.allow_done_sentinel)
            .unwrap_or(true);
        let persistent_system_prompt = if persistent {
            if exit_when_verified {
                if allow_done_sentinel {
                    Some(format!(
                        "\n\nKeep working until the task is complete. Take action with tool calls — \
                         do not stop to explain. Emit `<done>{done_sentinel}</done>` only after a \
                         passing verification run."
                    ))
                } else {
                    Some(
                        "\n\nKeep working until the task is complete. Take action with tool calls — \
                         do not stop to explain."
                            .to_string(),
                    )
                }
            } else if allow_done_sentinel {
                Some(format!(
                    "\n\nIMPORTANT: You MUST keep working until the task is complete. \
                     Do NOT stop to explain or summarize — take action with tool calls. \
                     When the requested work is complete, emit `<done>{done_sentinel}</done>` \
                     as its own top-level block."
                ))
            } else {
                Some(
                    "\n\nIMPORTANT: You MUST keep working until the task is complete. \
                     Do NOT stop to explain or summarize — take action with tool calls."
                        .to_string(),
                )
            }
        } else {
            None
        };
        let mut visible_messages = opts.messages.clone();
        let mut recorded_messages = opts.messages.clone();
        // Emit `message` events for the initial payload so transcript
        // replayers / LoRA corpus extractors see the opening context.
        for message in &opts.messages {
            crate::llm::agent_observe::emit_message_event(message);
        }

        let mut total_text = String::new();
        let mut last_iteration_text = String::new();
        let consecutive_text_only = 0usize;
        let consecutive_single_tool_turns = 0usize;
        let task_ledger = config.task_ledger.clone();
        let ledger_done_rejections = 0usize;
        let mut all_tools_used: Vec<String> = Vec::new();
        let successful_tools_used: Vec<String> = Vec::new();
        let mut rejected_tools: Vec<String> = Vec::new();
        let mut deferred_user_messages: Vec<String> = Vec::new();
        let mut total_iterations = 0usize;
        let final_status = "done";
        let mut transcript_summary = opts.transcript_summary.clone();
        let loop_start = std::time::Instant::now();
        let mut transcript_events: Vec<crate::value::VmValue> = Vec::new();
        let mut idle_backoff_ms = 100u64;
        let mut daemon_state = if daemon {
            "active".to_string()
        } else {
            "done".to_string()
        };
        let mut daemon_snapshot_path: Option<String> = None;
        let mut daemon_watch_state = watch_state(&daemon_config.watch_paths);
        let mut resumed_iterations = 0usize;
        let mut last_run_exit_code = last_run_exit_code;

        if daemon {
            if let Some(path) = daemon_config.resume_path.as_deref() {
                let snapshot = crate::llm::daemon::load_snapshot(path)?;
                daemon_state = snapshot.daemon_state.clone();
                visible_messages = snapshot.visible_messages;
                recorded_messages = snapshot.recorded_messages;
                transcript_summary = snapshot.transcript_summary;
                transcript_events = snapshot
                    .transcript_events
                    .iter()
                    .map(crate::stdlib::json_to_vm_value)
                    .collect();
                total_text = snapshot.total_text;
                last_iteration_text = snapshot.last_iteration_text;
                all_tools_used = snapshot.all_tools_used;
                rejected_tools = snapshot.rejected_tools;
                deferred_user_messages = snapshot.deferred_user_messages;
                resumed_iterations = snapshot.total_iterations;
                total_iterations = resumed_iterations;
                idle_backoff_ms = snapshot.idle_backoff_ms.max(1);
                last_run_exit_code = snapshot.last_run_exit_code;
                daemon_watch_state = if snapshot.watch_state.is_empty() {
                    watch_state(&daemon_config.watch_paths)
                } else {
                    snapshot.watch_state
                };
                daemon_snapshot_path = Some(path.to_string());
            } else if let Some(path) = daemon_config.effective_persist_path() {
                daemon_snapshot_path = Some(path.to_string());
            }
        }

        let rehydrated_active_skills = rehydrate_active_skills(
            anonymous_session,
            &session_id,
            config_skill_registry.as_ref(),
        );

        Ok(Self {
            config,
            session_id,
            anonymous_session,
            tool_contract_prompt,
            base_system,
            persistent_system_prompt,
            has_tools,
            visible_messages,
            recorded_messages,
            transcript_events,
            transcript_summary,
            total_text,
            last_iteration_text,
            task_ledger,
            ledger_done_rejections,
            loop_tracker,
            loop_detect_enabled,
            total_iterations,
            resumed_iterations,
            consecutive_text_only,
            consecutive_single_tool_turns,
            idle_backoff_ms,
            last_run_exit_code,
            all_tools_used,
            successful_tools_used,
            rejected_tools,
            deferred_user_messages,
            daemon_state,
            daemon_snapshot_path,
            daemon_watch_state,
            final_status,
            loop_start,
            bridge,
            tool_format,
            done_sentinel,
            break_unless_phase,
            max_iterations,
            max_nudges,
            tool_retries,
            tool_backoff_ms,
            exit_when_verified,
            persistent,
            daemon,
            auto_compact,
            daemon_config,
            custom_nudge,
            tool_search_client,
            rehydrated_from_session: !rehydrated_active_skills.is_empty(),
            active_skills: rehydrated_active_skills,
            loaded_skills: Vec::new(),
            skill_registry: config_skill_registry,
            skill_match: config_skill_match,
            working_files: config_working_files,
            native_tools_snapshot,
            _approval_guard,
            _policy_guard,
            _sink_guard,
            _iteration_guard,
        })
    }
}
