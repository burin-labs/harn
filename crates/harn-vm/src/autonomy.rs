use std::cell::RefCell;
use std::collections::BTreeMap;
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
    current_identity(&action).is_some_and(|identity| identity.tier != AutonomyTier::ActAuto)
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
    Some(AutonomyIdentity {
        agent_id,
        trace_id,
        tier,
        reviewers,
    })
}

fn detail_for(action: SideEffectAction, args: &[VmValue]) -> JsonValue {
    serde_json::json!({
        "builtin": action.builtin,
        "action_class": action.class,
        "args": args.iter().map(crate::llm::vm_value_to_json).collect::<Vec<_>>(),
    })
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
    record.metadata.insert(
        "autonomy.enforcement".to_string(),
        serde_json::json!(match identity.tier {
            AutonomyTier::Shadow => "shadow_noop",
            AutonomyTier::Suggest => "suggest_approval_request",
            AutonomyTier::ActWithApproval => "approval_granted",
            AutonomyTier::ActAuto => "auto",
        }),
    );
    record
        .metadata
        .insert("builtin".to_string(), serde_json::json!(action.builtin));
    record
        .metadata
        .insert("action_class".to_string(), serde_json::json!(action.class));
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
