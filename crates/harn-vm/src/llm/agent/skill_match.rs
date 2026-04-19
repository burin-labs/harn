//! Skill-matching phase.
//!
//! Resolves a concrete set of skills to activate for an agent_loop turn
//! from a skill registry (created with `skill_registry()` / `skill { }`
//! decls), the user's latest prompt, and the host-supplied working file
//! set. The phase runs once on iteration 0 and again (reassess) after
//! every turn when `sticky: false`. Activation binds a skill's
//! `prompt`, `allowed_tools`, and metadata onto `AgentLoopState` so the
//! preflight + tool-dispatch phases can react.
//!
//! Three strategies are supported:
//!
//! - `"metadata"` (default): BM25-ish term scoring over
//!   `description` + `when_to_use` plus glob matching on `paths`
//!   against the working file set. All in-VM, no host round-trip.
//! - `"host"`: delegates scoring to the host via the `skill/match`
//!   bridge RPC. Useful for embedding-based matchers.
//! - `"embedding"`: alias for `"host"` today; kept separate so the
//!   language accepts Anthropic's canonical term.
//!
//! Matching is deliberately permissive: unknown strategies fall back
//! to `"metadata"` with a warning, and bridge errors surface as an
//! empty match set (loop proceeds without an active skill).

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::agent_events::AgentEvent;
use crate::bridge::HostBridge;
use crate::value::{VmError, VmValue};

use super::super::helpers::transcript_event;
use super::state::{ActiveSkill, AgentLoopState, SkillMatchConfig, SkillMatchStrategy};

/// Per-skill score with optional diagnostic string.
#[derive(Clone, Debug)]
pub(super) struct SkillCandidate {
    pub name: String,
    pub score: f64,
    pub reason: String,
}

/// Public entry: decide which skills (if any) to activate this turn.
/// Called from the agent loop before `turn_preflight`:
/// - on iteration 0 (always)
/// - on every iteration when `sticky: false`
///
/// Safe to call when no skill registry is configured — returns early.
pub(super) async fn run_skill_match(
    state: &mut AgentLoopState,
    opts: &crate::llm::api::LlmCallOptions,
    bridge: &Option<Rc<HostBridge>>,
    session_id: &str,
    iteration: usize,
    is_reassess: bool,
) -> Result<(), VmError> {
    let Some(registry) = state.skill_registry.clone() else {
        return Ok(());
    };
    let match_config = state.skill_match.clone();
    let skills = extract_skills(&registry);
    if skills.is_empty() {
        return Ok(());
    }
    // Read the most recent user-role message from state, not opts:
    // opts.messages reflects the previous turn's preflight snapshot,
    // while state.visible_messages carries every post-turn injection
    // that happened after that snapshot (runtime feedback, host
    // messages, inject_feedback calls).
    let prompt_text =
        latest_user_prompt_from_state(state).unwrap_or_else(|| latest_user_prompt(opts));
    let working_files = state.working_files.clone();

    let candidates = match match_config.strategy {
        SkillMatchStrategy::Metadata => score_metadata(&skills, &prompt_text, &working_files),
        SkillMatchStrategy::Host | SkillMatchStrategy::Embedding => {
            match score_via_bridge(
                bridge.as_deref(),
                &skills,
                &prompt_text,
                &working_files,
                &match_config.strategy,
            )
            .await
            {
                Ok(c) => c,
                Err(err) => {
                    crate::events::log_warn(
                        "agent.skill_match",
                        &format!("host strategy failed: {err}; falling back to metadata scoring"),
                    );
                    score_metadata(&skills, &prompt_text, &working_files)
                }
            }
        }
    };

    let mut ranked = candidates;
    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Emit a `skill_matched` transcript + agent event regardless of whether
    // anything activated — replayers need to see the zero-match case too.
    let candidates_json: Vec<serde_json::Value> = ranked
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "score": c.score,
                "reason": c.reason,
            })
        })
        .collect();
    state.transcript_events.push(transcript_event(
        "skill_matched",
        "system",
        "internal",
        "",
        Some(serde_json::json!({
            "strategy": match_config.strategy.as_str(),
            "iteration": iteration,
            "reassess": is_reassess,
            "candidates": candidates_json,
            "working_files": working_files,
        })),
    ));

    // Filter out sub-threshold scores so the top-N pick doesn't
    // activate a skill that didn't actually match anything.
    let top: Vec<&SkillCandidate> = ranked
        .iter()
        .filter(|c| {
            c.score > 0.0
                || matches!(
                    match_config.strategy,
                    SkillMatchStrategy::Host | SkillMatchStrategy::Embedding
                )
        })
        .take(match_config.top_n.max(1))
        .collect();

    let current_names: Vec<String> = state.active_skills.iter().map(|s| s.name.clone()).collect();
    let new_names: Vec<String> = top.iter().map(|c| c.name.to_string()).collect();

    if current_names == new_names {
        // No change — nothing to activate / deactivate.
        return Ok(());
    }

    // Deactivate previously-active skills that aren't in the new set.
    let keep: std::collections::BTreeSet<&str> = new_names.iter().map(|s| s.as_str()).collect();
    let deactivated: Vec<ActiveSkill> = state
        .active_skills
        .iter()
        .filter(|s| !keep.contains(s.name.as_str()))
        .cloned()
        .collect();
    for skill in &deactivated {
        super::emit_agent_event(&AgentEvent::SkillDeactivated {
            session_id: session_id.to_string(),
            skill_name: skill.name.clone(),
            iteration,
        })
        .await;
        state.transcript_events.push(transcript_event(
            "skill_deactivated",
            "system",
            "internal",
            &skill.name,
            Some(serde_json::json!({
                "name": skill.name,
                "iteration": iteration,
            })),
        ));
        // Release MCP server bindings — ref-counted, so another
        // active skill that needs the same server keeps it alive.
        for server in &skill.mcp_servers {
            if crate::mcp_registry::is_registered(server) {
                crate::mcp_registry::release(server);
                state.transcript_events.push(transcript_event(
                    "skill_mcp_unbound",
                    "system",
                    "internal",
                    &skill.name,
                    Some(serde_json::json!({
                        "skill": skill.name,
                        "server": server,
                    })),
                ));
            }
        }
        run_skill_hook(&registry, &skill.name, "on_deactivate").await?;
    }
    // Drop any keep-alive-expired lazy connections from the previous
    // turn before reactivating — keeps the registry tidy without adding
    // a background sweeper thread.
    crate::mcp_registry::sweep_expired();

    // Activate newly-matched skills.
    state
        .active_skills
        .retain(|s| keep.contains(s.name.as_str()));
    let existing: std::collections::BTreeSet<String> =
        state.active_skills.iter().map(|s| s.name.clone()).collect();
    for cand in &top {
        if existing.contains(&cand.name) {
            continue;
        }
        let Some(skill_entry) = find_skill_entry(&registry, &cand.name) else {
            continue;
        };
        let active = ActiveSkill::from_entry(&skill_entry);
        super::emit_agent_event(&AgentEvent::SkillActivated {
            session_id: session_id.to_string(),
            skill_name: active.name.clone(),
            iteration,
            reason: cand.reason.clone(),
        })
        .await;
        state.transcript_events.push(transcript_event(
            "skill_activated",
            "system",
            "internal",
            &active.name,
            Some(serde_json::json!({
                "name": active.name,
                "description": active.description,
                "iteration": iteration,
                "score": cand.score,
                "reason": cand.reason,
                "allowed_tools": active.allowed_tools,
            })),
        ));
        if !active.allowed_tools.is_empty() {
            super::emit_agent_event(&AgentEvent::SkillScopeTools {
                session_id: session_id.to_string(),
                skill_name: active.name.clone(),
                allowed_tools: active.allowed_tools.clone(),
            })
            .await;
            state.transcript_events.push(transcript_event(
                "skill_scope_tools",
                "system",
                "internal",
                &active.name,
                Some(serde_json::json!({
                    "name": active.name,
                    "allowed_tools": active.allowed_tools,
                })),
            ));
        }
        // Bring up any MCP servers the skill declares in
        // `requires_mcp` / `mcp`. Lazy servers boot here on first
        // activation; eager servers just bump the refcount. Failures
        // log-and-continue — a missing MCP server shouldn't tear down
        // the loop, the skill's own handler will error out later with
        // a clearer message.
        for server in &active.mcp_servers {
            match crate::mcp_registry::ensure_active(server).await {
                Ok(_) => {
                    state.transcript_events.push(transcript_event(
                        "skill_mcp_bound",
                        "system",
                        "internal",
                        &active.name,
                        Some(serde_json::json!({
                            "skill": active.name,
                            "server": server,
                        })),
                    ));
                }
                Err(err) => {
                    crate::events::log_warn(
                        "agent.skill_mcp",
                        &format!(
                            "skill={} requires MCP server '{}' but activation failed: {}",
                            active.name, server, err
                        ),
                    );
                    state.transcript_events.push(transcript_event(
                        "skill_mcp_bind_failed",
                        "system",
                        "internal",
                        &active.name,
                        Some(serde_json::json!({
                            "skill": active.name,
                            "server": server,
                            "error": err.to_string(),
                        })),
                    ));
                }
            }
        }
        run_skill_hook(&registry, &active.name, "on_activate").await?;
        state.active_skills.push(active);
    }

    Ok(())
}

/// Flatten a skill registry (validated `skill_registry` dict) into a
/// `Vec<VmValue::Dict>` skill-entry list. Returns empty when the input
/// isn't recognisable as a registry.
fn extract_skills(registry: &VmValue) -> Vec<VmValue> {
    let Some(dict) = registry.as_dict() else {
        return Vec::new();
    };
    match dict.get("skills") {
        Some(VmValue::List(list)) => list.iter().cloned().collect(),
        _ => Vec::new(),
    }
}

/// Find the raw skill entry (`VmValue::Dict`) by name.
fn find_skill_entry(registry: &VmValue, name: &str) -> Option<VmValue> {
    for skill in extract_skills(registry) {
        if let VmValue::Dict(dict) = &skill {
            if dict
                .get("name")
                .map(|v| v.display() == name)
                .unwrap_or(false)
            {
                return Some(skill);
            }
        }
    }
    None
}

/// Execute an `on_activate` / `on_deactivate` closure if it's present
/// on the skill entry. Hook errors log-and-continue — a broken hook
/// must not tear down the loop.
async fn run_skill_hook(
    registry: &VmValue,
    skill_name: &str,
    hook_key: &str,
) -> Result<(), VmError> {
    let Some(skill) = find_skill_entry(registry, skill_name) else {
        return Ok(());
    };
    let Some(dict) = skill.as_dict() else {
        return Ok(());
    };
    let Some(VmValue::Closure(closure)) = dict.get(hook_key).cloned() else {
        return Ok(());
    };
    let Some(mut vm) = crate::vm::clone_async_builtin_child_vm() else {
        return Ok(());
    };
    if let Err(err) = vm.call_closure_pub(&closure, &[], &[]).await {
        crate::events::log_warn(
            "agent.skill_hook",
            &format!("skill={skill_name} hook={hook_key} error: {err}"),
        );
    }
    Ok(())
}

fn list_of_strings(value: Option<&VmValue>) -> Vec<String> {
    match value {
        Some(VmValue::List(list)) => list.iter().map(|v| v.display()).collect(),
        _ => Vec::new(),
    }
}

/// Extract the most recent user-role message content from the outgoing
/// payload. Used as the "query" for metadata scoring and as the
/// `prompt` param for bridge-based matchers.
fn latest_user_prompt(opts: &crate::llm::api::LlmCallOptions) -> String {
    for message in opts.messages.iter().rev() {
        if message.get("role").and_then(|v| v.as_str()) == Some("user") {
            if let Some(content) = message.get("content").and_then(|v| v.as_str()) {
                return content.to_string();
            }
        }
    }
    String::new()
}

/// Search `state.visible_messages` for the most recent user message.
/// Preferred over `opts.messages` because visible_messages carries
/// every post-turn injection from the previous iteration, while
/// `opts.messages` only refreshes in `turn_preflight`. Returns `None`
/// when no user message is present (callers fall back to opts).
fn latest_user_prompt_from_state(state: &AgentLoopState) -> Option<String> {
    for message in state.visible_messages.iter().rev() {
        if message.get("role").and_then(|v| v.as_str()) == Some("user") {
            if let Some(content) = message.get("content").and_then(|v| v.as_str()) {
                return Some(content.to_string());
            }
        }
    }
    None
}

/// BM25-ish scorer over `description` + `when_to_use`, combined with
/// `paths:` glob matching. A path hit dominates — if the user is
/// editing a file the skill claims to own, that's a stronger signal
/// than any keyword overlap.
fn score_metadata(
    skills: &[VmValue],
    prompt: &str,
    working_files: &[String],
) -> Vec<SkillCandidate> {
    let tokens = tokenize_lower(prompt);
    let mut candidates = Vec::new();
    for skill in skills {
        // Skip skills the author marked off-limits to the model.
        if ActiveSkill::is_disabled_for_model(skill) {
            continue;
        }
        let Some(dict) = skill.as_dict() else {
            continue;
        };
        let name = dict.get("name").map(|v| v.display()).unwrap_or_default();
        let description = dict
            .get("description")
            .map(|v| v.display())
            .unwrap_or_default();
        let when_to_use = dict
            .get("when_to_use")
            .map(|v| v.display())
            .unwrap_or_default();
        let paths = list_of_strings(dict.get("paths"));

        let mut score = 0.0_f64;
        let mut reasons: Vec<String> = Vec::new();

        let keyword_hits =
            count_term_hits(&tokens, &description) + count_term_hits(&tokens, &when_to_use);
        if keyword_hits > 0 {
            // Normalize by (token count + BM25-ish saturation) so a
            // skill with a short, high-overlap description doesn't lose
            // to a long verbose one.
            let bm25 = (keyword_hits as f64) / (keyword_hits as f64 + 1.5);
            score += bm25;
            reasons.push(format!("{keyword_hits} keyword hit(s)"));
        }

        // Name-in-prompt is a very strong signal (explicit skill name
        // mention). Boost even above path hits.
        if !name.is_empty() && prompt.to_lowercase().contains(&name.to_lowercase()) {
            score += 2.0;
            reasons.push(format!("prompt mentions '{name}'"));
        }

        let path_hits = count_path_hits(&paths, working_files);
        if path_hits > 0 {
            score += 1.5 * (path_hits as f64);
            reasons.push(format!("{path_hits} path glob(s) matched"));
        }

        if score > 0.0 {
            let reason = if reasons.is_empty() {
                String::new()
            } else {
                reasons.join("; ")
            };
            candidates.push(SkillCandidate {
                name,
                score,
                reason,
            });
        }
    }
    candidates
}

fn tokenize_lower(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 2)
        .map(|t| t.to_lowercase())
        .collect()
}

fn count_term_hits(terms: &[String], haystack: &str) -> usize {
    if terms.is_empty() || haystack.is_empty() {
        return 0;
    }
    let lower = haystack.to_lowercase();
    terms
        .iter()
        .filter(|term| lower.contains(term.as_str()))
        .count()
}

/// Count working files matched by any path glob on the skill. Globs
/// support `*` (single segment) and `**` (multi-segment) with the
/// semantics of ordinary shell globbing. Absolute or workspace-relative
/// paths are both accepted — matching is string-based.
fn count_path_hits(patterns: &[String], working_files: &[String]) -> usize {
    let mut hits = 0;
    for pattern in patterns {
        for file in working_files {
            if glob_match(pattern, file) {
                hits += 1;
                break;
            }
        }
    }
    hits
}

/// Simple glob matcher supporting `*` (non-separator) and `**`
/// (cross-separator). Intentionally self-contained — the existing
/// workspace had two other glob matchers but both were tightly coupled
/// to their callers.
fn glob_match(pattern: &str, path: &str) -> bool {
    let pat_bytes = pattern.as_bytes();
    let path_bytes = path.as_bytes();
    glob_match_inner(pat_bytes, 0, path_bytes, 0)
}

fn glob_match_inner(pat: &[u8], mut pi: usize, path: &[u8], mut si: usize) -> bool {
    while pi < pat.len() {
        match pat[pi] {
            b'*' => {
                let double = pi + 1 < pat.len() && pat[pi + 1] == b'*';
                let next_pi = if double { pi + 2 } else { pi + 1 };
                // Skip trailing slash after `**/`
                let (next_pi, _after_double_slash) =
                    if double && next_pi < pat.len() && pat[next_pi] == b'/' {
                        (next_pi + 1, true)
                    } else {
                        (next_pi, false)
                    };
                if next_pi >= pat.len() {
                    // Trailing `*` or `**` matches the rest.
                    if double {
                        return true;
                    }
                    // Single-`*` does not cross `/`.
                    return !path[si..].contains(&b'/');
                }
                // Try every suffix of `path[si..]`.
                for try_si in si..=path.len() {
                    if !double {
                        // `*` does not cross `/`: abort as soon as we see a slash.
                        if path[si..try_si].contains(&b'/') {
                            break;
                        }
                    }
                    if glob_match_inner(pat, next_pi, path, try_si) {
                        return true;
                    }
                }
                return false;
            }
            b'?' => {
                if si >= path.len() || path[si] == b'/' {
                    return false;
                }
                pi += 1;
                si += 1;
            }
            c => {
                if si >= path.len() || path[si] != c {
                    return false;
                }
                pi += 1;
                si += 1;
            }
        }
    }
    si == path.len()
}

async fn score_via_bridge(
    bridge: Option<&HostBridge>,
    skills: &[VmValue],
    prompt: &str,
    working_files: &[String],
    strategy: &SkillMatchStrategy,
) -> Result<Vec<SkillCandidate>, VmError> {
    let Some(bridge) = bridge else {
        return Err(VmError::Runtime(
            "skill_match strategy=\"host\" requires a host bridge".to_string(),
        ));
    };
    let candidate_meta: Vec<serde_json::Value> = skills
        .iter()
        .filter_map(|s| s.as_dict())
        .map(|d| {
            serde_json::json!({
                "name": d.get("name").map(|v| v.display()).unwrap_or_default(),
                "description": d.get("description").map(|v| v.display()).unwrap_or_default(),
                "when_to_use": d.get("when_to_use").map(|v| v.display()).unwrap_or_default(),
                "paths": list_of_strings(d.get("paths")),
            })
        })
        .collect();
    let params = serde_json::json!({
        "strategy": strategy.as_str(),
        "prompt": prompt,
        "working_files": working_files,
        "candidates": candidate_meta,
    });
    let response = bridge.call("skill/match", params).await?;
    let list = response
        .get("matches")
        .or_else(|| response.get("skills"))
        .or_else(|| response.get("result").and_then(|r| r.get("matches")))
        .cloned()
        .or_else(|| {
            if response.is_array() {
                Some(response)
            } else {
                None
            }
        })
        .unwrap_or(serde_json::Value::Array(Vec::new()));
    let arr = list.as_array().cloned().unwrap_or_default();
    let mut out = Vec::new();
    for entry in arr {
        let name = entry
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            continue;
        }
        let score = entry.get("score").and_then(|v| v.as_f64()).unwrap_or(1.0);
        let reason = entry
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("host match")
            .to_string();
        out.push(SkillCandidate {
            name,
            score,
            reason,
        });
    }
    Ok(out)
}

/// Parse the `skills:` / `skill_match:` / `working_files:` options off
/// the agent_loop options dict into concrete typed state. Called from
/// `AgentLoopConfig` parsing. When the caller does not provide
/// `skills:` / `skill_match:`, fall back to the workflow-level context
/// installed by `workflow_execute(...)` so nested delegation keeps the
/// same skill firewall unless it opts out explicitly.
pub fn parse_skill_config(
    options: &Option<BTreeMap<String, VmValue>>,
) -> (Option<VmValue>, SkillMatchConfig, Vec<String>) {
    let opts = options.as_ref();
    let workflow_context = crate::orchestration::current_workflow_skill_context();
    let skill_registry = match opts.and_then(|opts| opts.get("skills")) {
        Some(value) => normalize_skill_registry(value),
        None => workflow_context
            .as_ref()
            .and_then(|ctx| ctx.registry.clone()),
    };
    let skill_match = match opts.and_then(|opts| opts.get("skill_match")) {
        Some(value) => value
            .as_dict()
            .map(parse_skill_match_config)
            .unwrap_or_default(),
        None => workflow_context
            .as_ref()
            .and_then(|ctx| ctx.match_config.as_ref())
            .and_then(|value| value.as_dict())
            .map(parse_skill_match_config)
            .unwrap_or_default(),
    };
    let working_files = match opts.and_then(|opts| opts.get("working_files")) {
        Some(VmValue::List(list)) => list.iter().map(|v| v.display()).collect(),
        Some(VmValue::String(s)) => vec![s.to_string()],
        _ => Vec::new(),
    };
    (skill_registry, skill_match, working_files)
}

/// Accept a registry dict, a list of skill entries, or a list of
/// skill-name strings. The last form requires the registry to be
/// supplied separately, so it falls through to `None` today — the
/// issue spec names it as a future shape.
fn normalize_skill_registry(value: &VmValue) -> Option<VmValue> {
    match value {
        VmValue::Dict(d)
            if d.get("_type")
                .map(|v| v.display() == "skill_registry")
                .unwrap_or(false) =>
        {
            Some(value.clone())
        }
        VmValue::List(list) => {
            // Wrap a list of skill entries into a synthetic registry.
            let mut dict = BTreeMap::new();
            dict.insert(
                "_type".to_string(),
                VmValue::String(Rc::from("skill_registry")),
            );
            dict.insert("skills".to_string(), VmValue::List(list.clone()));
            Some(VmValue::Dict(Rc::new(dict)))
        }
        _ => None,
    }
}

/// Workflow-level entry point used by orchestration to parse a
/// `skill_match:` dict lifted from the workflow's `run_options`.
pub fn parse_skill_match_config_public(dict: &BTreeMap<String, VmValue>) -> SkillMatchConfig {
    parse_skill_match_config(dict)
}

fn parse_skill_match_config(dict: &BTreeMap<String, VmValue>) -> SkillMatchConfig {
    let strategy = dict
        .get("strategy")
        .map(|v| v.display())
        .map(|s| SkillMatchStrategy::parse(&s))
        .unwrap_or_default();
    let top_n = dict
        .get("top_n")
        .and_then(|v| v.as_int())
        .map(|n| n.max(1) as usize)
        .unwrap_or(1);
    let sticky = dict
        .get("sticky")
        .map(|v| matches!(v, VmValue::Bool(true)))
        .unwrap_or(true);
    SkillMatchConfig {
        strategy,
        top_n,
        sticky,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::{
        install_workflow_skill_context, WorkflowSkillContext, WorkflowSkillContextGuard,
    };

    fn test_skill_registry(name: &str) -> VmValue {
        VmValue::Dict(Rc::new(BTreeMap::from([
            (
                "_type".to_string(),
                VmValue::String(Rc::from("skill_registry")),
            ),
            (
                "skills".to_string(),
                VmValue::List(Rc::new(vec![VmValue::Dict(Rc::new(BTreeMap::from([(
                    "name".to_string(),
                    VmValue::String(Rc::from(name.to_string())),
                )])))])),
            ),
        ])))
    }

    fn first_skill_name(registry: &VmValue) -> Option<String> {
        registry
            .as_dict()
            .and_then(|dict| dict.get("skills"))
            .and_then(|skills| match skills {
                VmValue::List(list) => Some(list),
                _ => None,
            })
            .and_then(|skills| skills.first())
            .and_then(|skill| skill.as_dict())
            .and_then(|skill| skill.get("name"))
            .map(VmValue::display)
    }

    #[test]
    fn glob_basic_match() {
        assert!(glob_match("src/*.rs", "src/main.rs"));
        assert!(!glob_match("src/*.rs", "src/sub/main.rs"));
        assert!(glob_match("src/**/*.rs", "src/sub/dir/main.rs"));
        assert!(glob_match("Dockerfile", "Dockerfile"));
        assert!(!glob_match("Dockerfile", "Dockerfile.dev"));
        assert!(glob_match("infra/**", "infra/terraform/main.tf"));
    }

    #[test]
    fn tokenize_skips_short_and_punct() {
        let tokens = tokenize_lower("Deploy the AI service (to prod)!");
        assert!(tokens.contains(&"deploy".to_string()));
        assert!(tokens.contains(&"service".to_string()));
        assert!(tokens.contains(&"prod".to_string()));
        assert!(!tokens.contains(&"ai".to_string())); // too short (len 2)
        assert!(!tokens.contains(&"to".to_string())); // too short
    }

    #[test]
    fn score_metadata_ranks_prompt_mentions_highest() {
        use std::rc::Rc;
        let skill_a = VmValue::Dict(Rc::new(BTreeMap::from([
            ("name".to_string(), VmValue::String(Rc::from("deploy"))),
            (
                "description".to_string(),
                VmValue::String(Rc::from("Deploy the application to production")),
            ),
            (
                "when_to_use".to_string(),
                VmValue::String(Rc::from("User says deploy/ship/release")),
            ),
        ])));
        let skill_b = VmValue::Dict(Rc::new(BTreeMap::from([
            ("name".to_string(), VmValue::String(Rc::from("test"))),
            (
                "description".to_string(),
                VmValue::String(Rc::from("Run unit tests")),
            ),
        ])));
        let skills = vec![skill_a, skill_b];
        let ranked = score_metadata(&skills, "Please deploy the staging service", &[]);
        assert_eq!(ranked[0].name, "deploy");
    }

    #[test]
    fn score_metadata_path_hit_beats_nothing() {
        use std::rc::Rc;
        let skill = VmValue::Dict(Rc::new(BTreeMap::from([
            ("name".to_string(), VmValue::String(Rc::from("infra"))),
            (
                "description".to_string(),
                VmValue::String(Rc::from("Infrastructure work")),
            ),
            (
                "paths".to_string(),
                VmValue::List(Rc::new(vec![
                    VmValue::String(Rc::from("infra/**")),
                    VmValue::String(Rc::from("Dockerfile")),
                ])),
            ),
        ])));
        let working = vec!["infra/terraform/main.tf".to_string()];
        let ranked = score_metadata(&[skill], "unrelated prompt", &working);
        assert_eq!(ranked.len(), 1);
        assert!(ranked[0].score >= 1.5);
        assert!(ranked[0].reason.contains("path"));
    }

    #[test]
    fn disable_model_invocation_filters_out() {
        use std::rc::Rc;
        let skill = VmValue::Dict(Rc::new(BTreeMap::from([
            ("name".to_string(), VmValue::String(Rc::from("secret"))),
            (
                "description".to_string(),
                VmValue::String(Rc::from("Private skill")),
            ),
            ("disable-model-invocation".to_string(), VmValue::Bool(true)),
        ])));
        let ranked = score_metadata(&[skill], "private secret thing", &[]);
        assert!(ranked.is_empty());
    }

    #[test]
    fn parse_skill_config_falls_back_to_workflow_context() {
        install_workflow_skill_context(Some(WorkflowSkillContext {
            registry: Some(test_skill_registry("workflow-skill")),
            match_config: Some(VmValue::Dict(Rc::new(BTreeMap::from([
                ("strategy".to_string(), VmValue::String(Rc::from("host"))),
                ("top_n".to_string(), VmValue::Int(2)),
                ("sticky".to_string(), VmValue::Bool(false)),
            ])))),
        }));
        let _guard = WorkflowSkillContextGuard;

        let (skill_registry, skill_match, working_files) =
            parse_skill_config(&Some(BTreeMap::new()));

        let skill_registry = skill_registry.expect("workflow registry should be inherited");
        assert_eq!(
            first_skill_name(&skill_registry).as_deref(),
            Some("workflow-skill")
        );
        assert!(matches!(skill_match.strategy, SkillMatchStrategy::Host));
        assert_eq!(skill_match.top_n, 2);
        assert!(!skill_match.sticky);
        assert!(working_files.is_empty());
    }

    #[test]
    fn parse_skill_config_prefers_explicit_options() {
        install_workflow_skill_context(Some(WorkflowSkillContext {
            registry: Some(test_skill_registry("workflow-skill")),
            match_config: Some(VmValue::Dict(Rc::new(BTreeMap::from([(
                "strategy".to_string(),
                VmValue::String(Rc::from("host")),
            )])))),
        }));
        let _guard = WorkflowSkillContextGuard;

        let options = Some(BTreeMap::from([
            ("skills".to_string(), test_skill_registry("explicit-skill")),
            (
                "skill_match".to_string(),
                VmValue::Dict(Rc::new(BTreeMap::from([
                    (
                        "strategy".to_string(),
                        VmValue::String(Rc::from("metadata")),
                    ),
                    ("top_n".to_string(), VmValue::Int(3)),
                    ("sticky".to_string(), VmValue::Bool(true)),
                ]))),
            ),
            (
                "working_files".to_string(),
                VmValue::List(Rc::new(vec![VmValue::String(Rc::from("src/lib.rs"))])),
            ),
        ]));

        let (skill_registry, skill_match, working_files) = parse_skill_config(&options);

        let skill_registry = skill_registry.expect("explicit registry should win");
        assert_eq!(
            first_skill_name(&skill_registry).as_deref(),
            Some("explicit-skill")
        );
        assert!(matches!(skill_match.strategy, SkillMatchStrategy::Metadata));
        assert_eq!(skill_match.top_n, 3);
        assert!(skill_match.sticky);
        assert_eq!(working_files, vec!["src/lib.rs".to_string()]);
    }
}
