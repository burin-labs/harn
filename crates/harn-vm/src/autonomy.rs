use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use uuid::Uuid;

use crate::event_log::{active_event_log, EventLog, LogEvent, Topic};
use crate::stdlib::hitl::append_approval_request_on;
use crate::triggers::dispatcher::current_dispatch_context;
use crate::trust_graph::{append_trust_record, AutonomyTier, TrustOutcome, TrustRecord};
use crate::value::{categorized_error, ErrorCategory, VmError, VmValue};

/// Stable diagnostic prefix for a deny driven by the `needs-human` autonomy
/// class. Approval surfaces (Slack, IDE, portal) match on this code to render
/// the deny distinctly from a normal tier-based block.
pub const HARN_AUT_NEEDS_HUMAN_CODE: &str = "HARN-AUT-NEEDS-HUMAN";

/// Canonical string value of the `needs-human` autonomy class. Mirrors
/// `RepairSafety::NeedsHuman.as_str()` from `harn-parser` so the autonomy
/// surface and the repair-safety surface stay in lockstep.
pub const NEEDS_HUMAN_AUTONOMY_CLASS: &str = "needs-human";

thread_local! {
    static AUTONOMY_POLICY_STACK: RefCell<Vec<AutonomyPolicy>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct AutonomyPolicy {
    pub agent_id: Option<String>,
    pub autonomy_tier: Option<AutonomyTier>,
    pub tier: Option<AutonomyTier>,
    pub action_tiers: BTreeMap<String, AutonomyTier>,
    pub agent_tiers: BTreeMap<String, AutonomyTier>,
    pub agent_action_tiers: BTreeMap<String, BTreeMap<String, AutonomyTier>>,
    pub reviewers: Vec<String>,
    /// Mark the whole policy as `needs-human`: every side-effecting builtin
    /// covered by this policy raises a structured `HARN-AUT-NEEDS-HUMAN`
    /// deny, regardless of the resolved `autonomy_tier`.
    #[serde(default)]
    pub requires_human: bool,
    /// Per-action or per-class needs-human tags. Entries match either the
    /// builtin name (`write_file`) or the action class (`fs.write`).
    /// Mutually exclusive with auto-apply: an entry here always wins over
    /// any tier resolution.
    #[serde(default, alias = "action_requires_human")]
    pub requires_human_actions: BTreeSet<String>,
    /// Per-agent needs-human tags. If an agent is listed here, every side
    /// effect it attempts is treated as needs-human.
    #[serde(default)]
    pub requires_human_agents: BTreeSet<String>,
}

impl AutonomyPolicy {
    fn effective_tier_for(
        &self,
        agent_id: &str,
        action: &SideEffectAction,
    ) -> Option<AutonomyTier> {
        self.agent_action_tiers
            .get(agent_id)
            .and_then(|tiers| {
                tiers
                    .get(action.builtin)
                    .or_else(|| tiers.get(action.class))
                    .copied()
            })
            .or_else(|| self.agent_tiers.get(agent_id).copied())
            .or_else(|| {
                self.action_tiers
                    .get(action.builtin)
                    .or_else(|| self.action_tiers.get(action.class))
                    .copied()
            })
            .or(self.autonomy_tier)
            .or(self.tier)
    }

    /// Resolve whether a given (agent, action) is tagged `needs-human` under
    /// this policy. Any positive signal — blanket `requires_human`, a
    /// per-agent tag, or a per-builtin/per-class tag — flips the action into
    /// the needs-human discipline.
    fn is_needs_human(&self, agent_id: &str, action: &SideEffectAction) -> bool {
        if self.requires_human {
            return true;
        }
        if self.requires_human_agents.contains(agent_id) {
            return true;
        }
        if self.requires_human_actions.contains(action.builtin)
            || self.requires_human_actions.contains(action.class)
        {
            return true;
        }
        false
    }
}

fn action(
    builtin: &'static str,
    class: &'static str,
    capability: &'static str,
) -> SideEffectAction {
    SideEffectAction {
        builtin,
        class,
        capability,
    }
}

fn workspace_write_action(builtin: &'static str, class: &'static str) -> SideEffectAction {
    action(builtin, class, "workspace.write_text")
}

fn first_matching_action(
    name: &str,
    builtins: &[&'static str],
    class: &'static str,
    capability: &'static str,
) -> Option<SideEffectAction> {
    builtins
        .iter()
        .find(|builtin| **builtin == name)
        .map(|builtin| action(builtin, class, capability))
}

fn first_workspace_write_action(
    name: &str,
    builtins: &[&'static str],
    class: &'static str,
) -> Option<SideEffectAction> {
    builtins
        .iter()
        .find(|builtin| **builtin == name)
        .map(|builtin| workspace_write_action(builtin, class))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SideEffectAction {
    pub builtin: &'static str,
    pub class: &'static str,
    pub capability: &'static str,
}

#[derive(Clone, Debug)]
struct AutonomyIdentity {
    agent_id: String,
    trace_id: String,
    tier: AutonomyTier,
    reviewers: Vec<String>,
    /// Whether this (agent, action) is tagged `needs-human` by the
    /// currently-active autonomy policy. When true the dispatcher MUST
    /// deny auto-apply regardless of `tier`.
    requires_human: bool,
}

#[derive(Clone, Debug)]
pub enum AutonomyDecision {
    Skip(VmValue),
    AllowApproved,
}

pub struct AutonomyPolicyGuard;

impl Drop for AutonomyPolicyGuard {
    fn drop(&mut self) {
        AUTONOMY_POLICY_STACK.with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}

pub fn push_autonomy_policy(policy: AutonomyPolicy) -> AutonomyPolicyGuard {
    AUTONOMY_POLICY_STACK.with(|stack| stack.borrow_mut().push(policy));
    AutonomyPolicyGuard
}

pub fn current_autonomy_policy() -> Option<AutonomyPolicy> {
    AUTONOMY_POLICY_STACK.with(|stack| stack.borrow().last().cloned())
}

pub fn is_side_effecting_builtin(name: &str) -> bool {
    side_effect_action_for_builtin(name).is_some()
}

pub fn needs_async_side_effect_enforcement(name: &str) -> bool {
    let Some(action) = side_effect_action_for_builtin(name) else {
        return false;
    };
    current_identity(&action)
        // `needs-human` always needs the async enforcement path so the
        // dispatcher can emit the structured deny + approval-request,
        // even when the resolved tier is `ActAuto`.
        .is_some_and(|identity| identity.requires_human || identity.tier != AutonomyTier::ActAuto)
}

pub fn enforce_builtin_side_effect_boxed<'a>(
    name: &'a str,
    args: &'a [VmValue],
) -> Pin<Box<dyn Future<Output = Result<Option<AutonomyDecision>, VmError>> + 'a>> {
    Box::pin(enforce_builtin_side_effect(name, args))
}

pub fn side_effect_action_for_builtin(name: &str) -> Option<SideEffectAction> {
    first_workspace_write_action(
        name,
        &["write_file", "write_file_bytes", "append_file"],
        "fs.write",
    )
    .or_else(|| first_workspace_write_action(name, &["mkdir"], "fs.mkdir"))
    .or_else(|| first_workspace_write_action(name, &["copy_file"], "fs.copy"))
    .or_else(|| first_matching_action(name, &["delete_file"], "fs.delete", "workspace.delete"))
    .or_else(|| first_workspace_write_action(name, &["move_file"], "fs.move"))
    .or_else(|| {
        first_matching_action(
            name,
            &["exec", "exec_at", "shell", "shell_at"],
            "process.exec",
            "process.exec",
        )
    })
    .or_else(|| first_matching_action(name, &["host_call"], "host.call", "host.call"))
    .or_else(|| {
        first_matching_action(
            name,
            &["store_set", "store_delete", "store_save", "store_clear"],
            "store.write",
            "store.write",
        )
    })
    .or_else(|| {
        first_matching_action(
            name,
            &[
                "metadata_set",
                "metadata_save",
                "metadata_refresh_hashes",
                "invalidate_facts",
                "path_metadata_set",
            ],
            "metadata.write",
            "metadata.write",
        )
    })
    .or_else(|| {
        first_matching_action(
            name,
            &["checkpoint", "checkpoint_delete", "checkpoint_clear"],
            "checkpoint.write",
            "checkpoint.write",
        )
    })
    .or_else(|| {
        first_matching_action(
            name,
            &[
                "sse_server_response",
                "sse_server_send",
                "sse_server_heartbeat",
                "sse_server_flush",
                "sse_server_close",
                "sse_server_cancel",
                "sse_server_mock_receive",
                "sse_server_mock_disconnect",
            ],
            "network.sse.write",
            "network.sse",
        )
    })
    .or_else(|| {
        first_matching_action(
            name,
            &[
                "__agent_state_write",
                "__agent_state_delete",
                "__agent_state_handoff",
            ],
            "agent_state.write",
            "agent_state.write",
        )
    })
    .or_else(|| first_matching_action(name, &["mcp_release"], "mcp.release", "mcp.release"))
    .or_else(|| {
        first_matching_action(
            name,
            &[
                "git.worktree.create",
                "git.worktree.remove",
                "git.fetch",
                "git.rebase",
                "git.push",
            ],
            "git.write",
            "git.write",
        )
    })
}

pub async fn enforce_builtin_side_effect(
    name: &str,
    args: &[VmValue],
) -> Result<Option<AutonomyDecision>, VmError> {
    let Some(action) = side_effect_action_for_builtin(name) else {
        return Ok(None);
    };
    let Some(identity) = current_identity(&action) else {
        return Ok(None);
    };
    // `needs-human` is a transverse discipline: it forbids auto-apply even
    // when the resolved tier is `ActAuto`. Check this *before* tier
    // dispatch so no tier can ever override it.
    if identity.requires_human {
        emit_proposal_event(identity.tier, action, args).await?;
        let request_id = append_needs_human_approval_request(&identity, action, args).await?;
        append_enforcement_record(
            &identity,
            action,
            args,
            TrustOutcome::Denied,
            Some(request_id.clone()),
        )
        .await?;
        return Err(needs_human_deny_error(&identity, action, &request_id));
    }
    match identity.tier {
        AutonomyTier::ActAuto => Ok(None),
        AutonomyTier::Shadow => {
            emit_proposal_event(identity.tier, action, args).await?;
            append_enforcement_record(&identity, action, args, TrustOutcome::Denied, None).await?;
            Ok(Some(AutonomyDecision::Skip(VmValue::Nil)))
        }
        AutonomyTier::Suggest => {
            emit_proposal_event(identity.tier, action, args).await?;
            let request_id = append_nonblocking_approval_request(&identity, action, args).await?;
            append_enforcement_record(
                &identity,
                action,
                args,
                TrustOutcome::Denied,
                Some(request_id),
            )
            .await?;
            Ok(Some(AutonomyDecision::Skip(VmValue::Nil)))
        }
        AutonomyTier::ActWithApproval => {
            let approval = request_approval_before_effect(&identity, action, args).await?;
            append_enforcement_record(
                &identity,
                action,
                args,
                TrustOutcome::Success,
                approval.request_id,
            )
            .await?;
            Ok(Some(AutonomyDecision::AllowApproved))
        }
    }
}

fn current_identity(action: &SideEffectAction) -> Option<AutonomyIdentity> {
    let scoped = current_autonomy_policy();
    let dispatch = current_dispatch_context();
    let agent_id = scoped
        .as_ref()
        .and_then(|policy| policy.agent_id.clone())
        .or_else(|| dispatch.as_ref().map(|context| context.agent_id.clone()))
        .unwrap_or_else(|| "runtime".to_string());
    let tier = scoped
        .as_ref()
        .and_then(|policy| policy.effective_tier_for(&agent_id, action))
        .or_else(|| dispatch.as_ref().map(|context| context.autonomy_tier))?;
    let trace_id = dispatch
        .as_ref()
        .map(|context| context.trigger_event.trace_id.0.clone())
        .unwrap_or_else(|| format!("trace-{}", Uuid::now_v7()));
    let reviewers = scoped
        .as_ref()
        .map(|policy| policy.reviewers.clone())
        .filter(|reviewers| !reviewers.is_empty())
        .unwrap_or_default();
    let requires_human = scoped
        .as_ref()
        .map(|policy| policy.is_needs_human(&agent_id, action))
        .unwrap_or(false);
    Some(AutonomyIdentity {
        agent_id,
        trace_id,
        tier,
        reviewers,
        requires_human,
    })
}

fn detail_for(action: SideEffectAction, args: &[VmValue]) -> JsonValue {
    serde_json::json!({
        "builtin": action.builtin,
        "action_class": action.class,
        "args": args.iter().map(crate::llm::vm_value_to_json).collect::<Vec<_>>(),
    })
}

fn needs_human_detail(action: SideEffectAction, args: &[VmValue]) -> JsonValue {
    let mut detail = detail_for(action, args);
    if let Some(obj) = detail.as_object_mut() {
        obj.insert(
            "autonomy_class".to_string(),
            JsonValue::String(NEEDS_HUMAN_AUTONOMY_CLASS.to_string()),
        );
        obj.insert("requires_human".to_string(), JsonValue::Bool(true));
        obj.insert(
            "deny_code".to_string(),
            JsonValue::String(HARN_AUT_NEEDS_HUMAN_CODE.to_string()),
        );
    }
    detail
}

async fn emit_proposal_event(
    tier: AutonomyTier,
    action: SideEffectAction,
    args: &[VmValue],
) -> Result<(), VmError> {
    let Some(context) = current_dispatch_context() else {
        return Ok(());
    };
    let Some(log) = active_event_log() else {
        return Ok(());
    };
    let topic = Topic::new(crate::TRIGGER_OUTBOX_TOPIC)
        .map_err(|error| VmError::Runtime(format!("autonomy proposal topic error: {error}")))?;
    let mut headers = BTreeMap::new();
    headers.insert(
        "trace_id".to_string(),
        context.trigger_event.trace_id.0.clone(),
    );
    headers.insert("agent".to_string(), context.agent_id.clone());
    headers.insert("autonomy_tier".to_string(), tier.as_str().to_string());
    let payload = serde_json::json!({
        "agent": context.agent_id,
        "action": context.action,
        "builtin": action.builtin,
        "action_class": action.class,
        "args": args.iter().map(crate::llm::vm_value_to_json).collect::<Vec<_>>(),
        "trace_id": context.trigger_event.trace_id.0,
        "replay_of_event_id": context.replay_of_event_id,
        "autonomy_tier": tier,
        "proposal": true,
    });
    log.append(
        &topic,
        LogEvent::new("dispatch_proposed", payload).with_headers(headers),
    )
    .await
    .map(|_| ())
    .map_err(|error| VmError::Runtime(format!("failed to append autonomy proposal: {error}")))
}

async fn append_nonblocking_approval_request(
    identity: &AutonomyIdentity,
    action: SideEffectAction,
    args: &[VmValue],
) -> Result<String, VmError> {
    let log = active_event_log().ok_or_else(|| {
        categorized_error(
            "autonomy approval requires an active event log",
            ErrorCategory::ToolRejected,
        )
    })?;
    append_approval_request_on(
        &log,
        identity.agent_id.clone(),
        identity.trace_id.clone(),
        action.class.to_string(),
        detail_for(action, args),
        identity.reviewers.clone(),
    )
    .await
}

/// Emit a non-blocking approval request tagged with the `needs-human`
/// autonomy class. Surfaces (Slack-approval, IDE, portal) match on the
/// `autonomy_class` field in the request payload's `detail` to render the
/// pending row distinctly from a normal tier-driven approval ask.
async fn append_needs_human_approval_request(
    identity: &AutonomyIdentity,
    action: SideEffectAction,
    args: &[VmValue],
) -> Result<String, VmError> {
    let log = active_event_log().ok_or_else(|| {
        categorized_error(
            "needs-human autonomy class requires an active event log",
            ErrorCategory::ToolRejected,
        )
    })?;
    append_approval_request_on(
        &log,
        identity.agent_id.clone(),
        identity.trace_id.clone(),
        format!("{}#needs-human", action.class),
        needs_human_detail(action, args),
        identity.reviewers.clone(),
    )
    .await
}

/// Build the structured deny returned when a `needs-human`-tagged side
/// effect is attempted. The message is prefixed with [`HARN_AUT_NEEDS_HUMAN_CODE`]
/// so approval surfaces and structured-error consumers can match on a stable
/// token rather than substring-matching the human-readable text.
fn needs_human_deny_error(
    identity: &AutonomyIdentity,
    action: SideEffectAction,
    request_id: &str,
) -> VmError {
    categorized_error(
        format!(
            "{code}: side effect `{builtin}` ({class}) is tagged `needs-human` for agent `{agent}`; \
             auto-apply is forbidden regardless of autonomy tier `{tier}`. \
             Approval request `{request_id}` was queued.",
            code = HARN_AUT_NEEDS_HUMAN_CODE,
            builtin = action.builtin,
            class = action.class,
            agent = identity.agent_id,
            tier = identity.tier.as_str(),
            request_id = request_id,
        ),
        ErrorCategory::ToolRejected,
    )
}

struct ApprovalOutcome {
    request_id: Option<String>,
}

async fn request_approval_before_effect(
    identity: &AutonomyIdentity,
    action: SideEffectAction,
    args: &[VmValue],
) -> Result<ApprovalOutcome, VmError> {
    active_event_log().ok_or_else(|| {
        categorized_error(
            "act_with_approval requires an active event log",
            ErrorCategory::ToolRejected,
        )
    })?;
    let detail = detail_for(action, args);
    let approval = crate::stdlib::hitl::request_approval_for_side_effect(
        action.class,
        detail,
        identity.agent_id.clone(),
        identity.reviewers.clone(),
        vec![action.capability.to_string()],
    )
    .await?;
    let request_id = approval
        .as_dict()
        .and_then(|dict| dict.get("request_id"))
        .map(VmValue::display);
    Ok(ApprovalOutcome { request_id })
}

async fn append_enforcement_record(
    identity: &AutonomyIdentity,
    action: SideEffectAction,
    args: &[VmValue],
    outcome: TrustOutcome,
    request_id: Option<String>,
) -> Result<(), VmError> {
    let Some(log) = active_event_log() else {
        return Ok(());
    };
    let mut record = TrustRecord::new(
        identity.agent_id.clone(),
        action.class.to_string(),
        None,
        outcome,
        identity.trace_id.clone(),
        identity.tier,
    );
    let enforcement = if identity.requires_human {
        // `needs-human` always denies regardless of tier — record the
        // distinct enforcement label so audit consumers can filter on it
        // without re-deriving the discipline from policy snapshots.
        "needs_human_denied"
    } else {
        match identity.tier {
            AutonomyTier::Shadow => "shadow_noop",
            AutonomyTier::Suggest => "suggest_approval_request",
            AutonomyTier::ActWithApproval => "approval_granted",
            AutonomyTier::ActAuto => "auto",
        }
    };
    record.metadata.insert(
        "autonomy.enforcement".to_string(),
        serde_json::json!(enforcement),
    );
    record
        .metadata
        .insert("builtin".to_string(), serde_json::json!(action.builtin));
    record
        .metadata
        .insert("action_class".to_string(), serde_json::json!(action.class));
    // Every record carries an explicit autonomy class so the trust-graph
    // record (`TrustRecord.metadata.autonomy_class`) flows downstream into
    // approval surfaces and receipt envelopes. `needs-human` is mutually
    // exclusive with the tier-based labels.
    let autonomy_class = if identity.requires_human {
        NEEDS_HUMAN_AUTONOMY_CLASS.to_string()
    } else {
        identity.tier.as_str().to_string()
    };
    record.metadata.insert(
        "autonomy_class".to_string(),
        serde_json::json!(autonomy_class),
    );
    record.metadata.insert(
        "requires_human".to_string(),
        serde_json::json!(identity.requires_human),
    );
    if identity.requires_human {
        record.metadata.insert(
            "deny_code".to_string(),
            serde_json::json!(HARN_AUT_NEEDS_HUMAN_CODE),
        );
    }
    record.metadata.insert(
        "args".to_string(),
        serde_json::json!(args
            .iter()
            .map(crate::llm::vm_value_to_json)
            .collect::<Vec<_>>()),
    );
    if let Some(request_id) = request_id {
        record.metadata.insert(
            "approval_request_id".to_string(),
            serde_json::json!(request_id),
        );
    }
    append_trust_record(&log, &record)
        .await
        .map(|_| ())
        .map_err(|error| VmError::Runtime(format!("autonomy trust graph append: {error}")))
}
