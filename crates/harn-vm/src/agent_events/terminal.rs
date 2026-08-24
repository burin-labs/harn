//! Typed terminal outcome for an agent-loop session (harn#4568).
//!
//! The loop seals a free-string `stop_reason` / `final_status`, which forces
//! every host to substring-match to tell a natural completion from a user
//! cancel, a provider or runtime error, a policy stop (budget / no-progress /
//! guardrail), or a suspend. That guessing is exactly how a catch-all terminal
//! message can hide a provider/VM/permission failure behind an agent-authored
//! "I stopped" (Burin #4642).
//!
//! This module produces the classification ONCE, at the loop boundary, into a
//! typed [`AgentTerminalKind`] plus a coarse [`AgentTerminalKind::owner`], and
//! carries it alongside the lossless raw `reason`. Harn owns the agent stop
//! vocabulary, so the classification is produced here rather than reconstructed
//! in Burin or any other host. The typed outcome is *additive*: the raw
//! `final_status` / `stop_reason` / `terminal_class` fields are unchanged.

use serde::{Deserialize, Serialize};

use crate::llm::AgentTerminalClass;

use super::agent::AgentEvent;

/// Coarse, typed classification of why an agent-loop session terminated.
/// Serialized `snake_case`. The vocabulary is deliberately extensible —
/// [`Self::Unknown`] is the honest fallback when no rule matched and the raw
/// `reason` is authoritative.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTerminalKind {
    /// The model/agent finished naturally — a clean completion or a verified
    /// `done` judgement, with no policy stop and no error.
    Natural,
    /// A user or host explicitly cancelled the in-flight turn.
    UserCancelled,
    /// A budget/cap policy stopped the loop: max iterations, a token/cost
    /// budget, or a circuit breaker.
    PolicyBudget,
    /// Completion was proposed but could not be verified before a judge
    /// deadline or policy limit. This is a failed completion, not a cancel.
    CompletionUnverified,
    /// The text-only nudge budget stopped a loop that did not issue actions.
    PolicyNoProgress,
    /// The stall governor stopped a loop after repeated equivalent actions or
    /// observations crossed its hard-stop threshold.
    PolicyThrash,
    /// A guardrail policy stopped the loop: an input tripwire or an
    /// out-of-scope alert.
    PolicyGuardrail,
    /// A custom post-turn / terminal callback requested a stop that is not one
    /// of the specific policy kinds above. The raw `reason` names it; a future
    /// typed callback contract can attribute a finer owner.
    PolicyStop,
    /// The provider/transport failed terminally: rate limit, timeout, context
    /// overflow, or provider misconfiguration.
    ProviderError,
    /// The harness/runtime failed terminally: a host-bridge gap, an internal
    /// protocol failure, an uncaught throw, or a turn that made no LLM call.
    RuntimeError,
    /// The session suspended at a waitpoint and may resume later — its work is
    /// not finished and was not abandoned.
    Suspended,
    /// No rule matched; the raw `reason` is authoritative.
    Unknown,
}

impl AgentTerminalKind {
    pub const ALL: [Self; 12] = [
        Self::Natural,
        Self::UserCancelled,
        Self::PolicyBudget,
        Self::CompletionUnverified,
        Self::PolicyNoProgress,
        Self::PolicyThrash,
        Self::PolicyGuardrail,
        Self::PolicyStop,
        Self::ProviderError,
        Self::RuntimeError,
        Self::Suspended,
        Self::Unknown,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Natural => "natural",
            Self::UserCancelled => "user_cancelled",
            Self::PolicyBudget => "policy_budget",
            Self::CompletionUnverified => "completion_unverified",
            Self::PolicyNoProgress => "policy_no_progress",
            Self::PolicyThrash => "policy_thrash",
            Self::PolicyGuardrail => "policy_guardrail",
            Self::PolicyStop => "policy_stop",
            Self::ProviderError => "provider_error",
            Self::RuntimeError => "runtime_error",
            Self::Suspended => "suspended",
            Self::Unknown => "unknown",
        }
    }

    /// Parse the stable wire value emitted in `AgentResult.terminal.kind`.
    pub fn from_wire(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == value.trim())
    }

    /// Project the producer-owned terminal decision onto the shared
    /// agent/run lifecycle vocabulary. Protocol and persistence adapters use
    /// this projection instead of independently interpreting status strings.
    pub const fn lifecycle_state(self) -> super::AgentLifecycleState {
        match self {
            Self::Natural => super::AgentLifecycleState::Completed,
            Self::UserCancelled => super::AgentLifecycleState::Cancelled,
            Self::PolicyBudget
            | Self::PolicyNoProgress
            | Self::PolicyThrash
            | Self::PolicyGuardrail
            | Self::PolicyStop => super::AgentLifecycleState::Stopped,
            Self::CompletionUnverified
            | Self::ProviderError
            | Self::RuntimeError
            | Self::Unknown => super::AgentLifecycleState::Failed,
            Self::Suspended => super::AgentLifecycleState::Suspended,
        }
    }

    /// The party responsible for the stop — a stable, coarse attribution that
    /// pairs with the kind so hosts can bucket outcomes (agent-driven vs
    /// user vs provider vs harness vs policy) without re-deriving it.
    pub fn owner(self) -> &'static str {
        match self {
            Self::Natural | Self::Suspended => "agent",
            Self::UserCancelled => "user",
            Self::PolicyBudget
            | Self::CompletionUnverified
            | Self::PolicyNoProgress
            | Self::PolicyThrash
            | Self::PolicyGuardrail
            | Self::PolicyStop => "policy",
            Self::ProviderError => "provider",
            Self::RuntimeError => "harness",
            Self::Unknown => "unknown",
        }
    }
}

/// Raw `stop_reason` values that seal a genuinely natural completion (a clean
/// finish or a verified `done`). When `final_status` is `done`/empty, any
/// `stop_reason` OUTSIDE this set is a policy/custom stop (e.g. a post-turn
/// callback `stop`) that must NOT be reported as a natural completion — that
/// conflation is the Burin #4642 failure mode. Sourced from the loop's own
/// terminal-`done` assignments and `__agent_loop_sealed_stop_reason`.
const NATURAL_STOP_REASONS: [&str; 9] = [
    "",
    "completed",
    "natural",
    "post_edit_reverify",
    "repeated_verified_pass",
    "required_tools_satisfied",
    "sentinel",
    "stalled_done_judge",
    "done",
];

/// Typed terminal outcome carried alongside the lossless raw reason. `owner` is
/// derived from `kind` so a single field pins the responsible party, and
/// `terminal_class` carries the finalize host's fine-grained reason when it
/// produced one.
///
/// The kind says who is responsible; the class says what went wrong. A host
/// needs both to say more than "the provider failed" — telling an unresolvable
/// credential from a rate limit is the difference between a remedy the user can
/// act on and one they cannot. The class rides on the `terminalClass` key the
/// failed-prompt frame already uses ([`AcpPromptErrorData`]), so one cause is
/// named the same way whichever side of the JSON-RPC boundary it comes back on.
///
/// Additive: absent on any outcome without a class, so a consumer that does not
/// read it sees byte-identical metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentTerminalOutcome {
    pub kind: AgentTerminalKind,
    pub reason: String,
    pub owner: String,
    #[serde(
        rename = "terminalClass",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub terminal_class: Option<AgentTerminalClass>,
}

impl AgentTerminalOutcome {
    /// Build an outcome from a kind and the lossless raw `reason`, deriving the
    /// `owner` from the kind.
    pub fn new(kind: AgentTerminalKind, reason: impl Into<String>) -> Self {
        Self {
            kind,
            reason: reason.into(),
            owner: kind.owner().to_string(),
            terminal_class: None,
        }
    }

    /// Attach the finalize host's fine-grained class. Separate from `new` so
    /// every existing construction keeps compiling and keeps its wire bytes,
    /// and only the boundary that actually computes a class carries one.
    #[must_use]
    pub fn with_terminal_class(mut self, terminal_class: Option<AgentTerminalClass>) -> Self {
        self.terminal_class = terminal_class;
        self
    }

    pub fn to_json(&self) -> serde_json::Value {
        let mut value = serde_json::json!({
            "kind": self.kind.as_str(),
            "reason": self.reason,
            "owner": self.owner,
        });
        if let Some(class) = self.terminal_class {
            value["terminalClass"] = serde_json::Value::String(class.as_str().to_string());
        }
        value
    }

    /// Project the outcome onto the existing typed-checkpoint event stream.
    pub fn checkpoint(
        &self,
        session_id: &str,
        final_status: &str,
        stop_reason: &str,
    ) -> AgentEvent {
        AgentEvent::TypedCheckpoint {
            session_id: session_id.to_owned(),
            checkpoint: serde_json::json!({
                "schema": "harn.agent_terminal.v1",
                "terminal": self.to_json(),
                "final_status": final_status,
                "stop_reason": stop_reason,
            }),
        }
    }
}

/// Build the terminal outcome a finalized turn reports, from the values the
/// finalize boundary has in hand.
///
/// One owner for four steps that were previously inlined at the call site:
/// classify the kind (using the class, so an errored turn is attributed rather
/// than left `Unknown`), keep the lossless raw reason, and carry the class
/// itself so a consumer can name the cause and not just the owner. Keeping them
/// together is what makes it visible when one of them is dropped.
pub fn terminal_outcome_for_finalize(
    canonical_status: &str,
    stop_reason: &str,
    terminal_class: Option<AgentTerminalClass>,
    has_error: bool,
) -> AgentTerminalOutcome {
    let reason = if stop_reason.is_empty() {
        canonical_status
    } else {
        stop_reason
    };
    AgentTerminalOutcome::new(
        classify_agent_terminal_with_class(
            canonical_status,
            stop_reason,
            has_error,
            terminal_class,
        ),
        reason,
    )
    .with_terminal_class(terminal_class)
}

/// Classify an agent-loop terminal condition into a typed [`AgentTerminalKind`].
///
/// Inputs are the values the finalize boundary already has in hand:
/// - `canonical_status`: `final_status` with empty normalized to `done`;
/// - `stop_reason`: the sealed raw stop reason;
/// - `has_error`: whether a terminal error was recorded;
/// - `terminal_class`: the finalize host's fine-grained error class, used only
///   to split an error into provider vs harness ownership.
///
/// This is the single place stringly stop vocabulary is interpreted; every
/// consumer reads the typed result instead.
pub fn classify_agent_terminal(
    canonical_status: &str,
    stop_reason: &str,
    has_error: bool,
    terminal_class: Option<&str>,
) -> AgentTerminalKind {
    classify_agent_terminal_with_class(
        canonical_status,
        stop_reason,
        has_error,
        terminal_class.and_then(AgentTerminalClass::from_wire),
    )
}

pub fn classify_agent_terminal_with_class(
    canonical_status: &str,
    stop_reason: &str,
    has_error: bool,
    terminal_class: Option<AgentTerminalClass>,
) -> AgentTerminalKind {
    match canonical_status {
        "suspended" => AgentTerminalKind::Suspended,
        // A user/host cancel. The finalize loop does not itself seal a
        // `cancelled` status — the ACP adapter observes the cancel notification
        // one layer up and constructs the outcome directly — but classify still
        // maps it so the vocabulary is total and any host that does route a
        // cancel through finalize is attributed correctly rather than `Unknown`.
        "cancelled" | "canceled" | "aborted" => AgentTerminalKind::UserCancelled,
        "provider_error" => AgentTerminalKind::ProviderError,
        "error" | "failed" => classify_error(terminal_class),
        // A verification cap/budget was exhausted before `done` could be
        // confirmed — a budget policy stop, not a hard error.
        "budget_exhausted" | "verify_exhausted" => AgentTerminalKind::PolicyBudget,
        "completion_unverified" => AgentTerminalKind::CompletionUnverified,
        "stuck" if stop_reason == "thrash_hard_stop" => AgentTerminalKind::PolicyThrash,
        "stuck" => AgentTerminalKind::PolicyNoProgress,
        // `input_guardrail`/`scope_alert` come from the loop's guardrail arms;
        // `blocked` is the UserPromptSubmit-hook block result. All three are a
        // guardrail policy denying the turn.
        "input_guardrail" | "scope_alert" | "blocked" => AgentTerminalKind::PolicyGuardrail,
        "done" => {
            if has_error {
                classify_error(terminal_class)
            } else if NATURAL_STOP_REASONS.contains(&stop_reason) {
                AgentTerminalKind::Natural
            } else {
                // `done`/empty final status with a non-natural reason — a
                // post-turn/custom policy stop wearing a completion status.
                AgentTerminalKind::PolicyStop
            }
        }
        // An unrecognized status is not the same as an unattributed outcome. A
        // pipeline is free to seal a status this table has never heard of, and
        // when that turn also recorded a terminal error the finalize host has
        // already classified it into a fine-grained `terminal_class`. Reporting
        // `Unknown` there throws that classification away and leaves an
        // embedder unable to tell a missing provider credential from a
        // tool-policy refusal — so its only honest presentation is "stopped for
        // an unknown reason", for a cause the harness knew all along.
        //
        // `Unknown` stays reserved for what it means: a terminal condition with
        // no error and no rule that matched, where the raw `reason` really is
        // the only fact there is.
        _ => {
            if has_error || terminal_class.is_some() {
                classify_error(terminal_class)
            } else {
                AgentTerminalKind::Unknown
            }
        }
    }
}

/// Split a terminal error into provider vs harness ownership using the
/// finalize host's error class. Transport/provider classes attribute to the
/// provider; everything else (host-bridge gaps, protocol failures, uncaught
/// throws) is a harness/runtime fault.
fn classify_error(terminal_class: Option<AgentTerminalClass>) -> AgentTerminalKind {
    if terminal_class.is_some_and(AgentTerminalClass::is_provider_error) {
        AgentTerminalKind::ProviderError
    } else {
        AgentTerminalKind::RuntimeError
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_wire_strings_are_stable_snake_case() {
        let pairs = [
            (AgentTerminalKind::Natural, "natural"),
            (AgentTerminalKind::UserCancelled, "user_cancelled"),
            (AgentTerminalKind::PolicyBudget, "policy_budget"),
            (
                AgentTerminalKind::CompletionUnverified,
                "completion_unverified",
            ),
            (AgentTerminalKind::PolicyNoProgress, "policy_no_progress"),
            (AgentTerminalKind::PolicyThrash, "policy_thrash"),
            (AgentTerminalKind::PolicyGuardrail, "policy_guardrail"),
            (AgentTerminalKind::PolicyStop, "policy_stop"),
            (AgentTerminalKind::ProviderError, "provider_error"),
            (AgentTerminalKind::RuntimeError, "runtime_error"),
            (AgentTerminalKind::Suspended, "suspended"),
            (AgentTerminalKind::Unknown, "unknown"),
        ];
        for (variant, wire) in pairs {
            assert_eq!(variant.as_str(), wire);
            let encoded = serde_json::to_string(&variant).unwrap();
            assert_eq!(encoded, format!("\"{wire}\""));
            let decoded: AgentTerminalKind = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, variant);
            assert_eq!(AgentTerminalKind::from_wire(wire), Some(variant));
        }
        // The wire table above must cover every variant.
        assert_eq!(pairs.len(), AgentTerminalKind::ALL.len());
    }

    #[test]
    fn terminal_kinds_project_to_shared_lifecycle_states() {
        use super::super::AgentLifecycleState;

        assert_eq!(
            AgentTerminalKind::Natural.lifecycle_state(),
            AgentLifecycleState::Completed
        );
        assert_eq!(
            AgentTerminalKind::Suspended.lifecycle_state(),
            AgentLifecycleState::Suspended
        );
        assert_eq!(
            AgentTerminalKind::UserCancelled.lifecycle_state(),
            AgentLifecycleState::Cancelled
        );
        for kind in [
            AgentTerminalKind::PolicyBudget,
            AgentTerminalKind::PolicyNoProgress,
            AgentTerminalKind::PolicyThrash,
            AgentTerminalKind::PolicyGuardrail,
            AgentTerminalKind::PolicyStop,
        ] {
            assert_eq!(kind.lifecycle_state(), AgentLifecycleState::Stopped);
        }
        for kind in [
            AgentTerminalKind::CompletionUnverified,
            AgentTerminalKind::ProviderError,
            AgentTerminalKind::RuntimeError,
            AgentTerminalKind::Unknown,
        ] {
            assert_eq!(kind.lifecycle_state(), AgentLifecycleState::Failed);
        }
    }

    #[test]
    fn owner_attribution_is_coarse_and_total() {
        for kind in AgentTerminalKind::ALL {
            let owner = kind.owner();
            assert!(
                matches!(
                    owner,
                    "agent" | "user" | "policy" | "provider" | "harness" | "unknown"
                ),
                "{kind:?} has an unexpected owner {owner}"
            );
        }
        assert_eq!(AgentTerminalKind::UserCancelled.owner(), "user");
        assert_eq!(AgentTerminalKind::ProviderError.owner(), "provider");
        assert_eq!(AgentTerminalKind::RuntimeError.owner(), "harness");
        assert_eq!(AgentTerminalKind::PolicyStop.owner(), "policy");
        assert_eq!(AgentTerminalKind::CompletionUnverified.owner(), "policy");
        assert_eq!(AgentTerminalKind::PolicyThrash.owner(), "policy");
    }

    #[test]
    fn natural_completion_classifies_as_natural() {
        for reason in [
            "",
            "completed",
            "natural",
            "post_edit_reverify",
            "repeated_verified_pass",
            "required_tools_satisfied",
            "sentinel",
        ] {
            assert_eq!(
                classify_agent_terminal("done", reason, false, None),
                AgentTerminalKind::Natural,
                "reason {reason:?} should be natural"
            );
        }
    }

    #[test]
    fn post_turn_policy_stop_is_not_reported_as_natural() {
        // The Burin #4642 case: a post-turn callback stops with `final_status`
        // empty (canonicalized to `done`) and a custom reason. It MUST classify
        // as a policy stop, never a natural completion.
        assert_eq!(
            classify_agent_terminal("done", "post_turn_stop", false, None),
            AgentTerminalKind::PolicyStop,
        );
        assert_eq!(
            classify_agent_terminal("done", "custom_operator_halt", false, None),
            AgentTerminalKind::PolicyStop,
        );
    }

    #[test]
    fn policy_stops_map_to_specific_kinds() {
        assert_eq!(
            classify_agent_terminal("budget_exhausted", "max_iterations", false, None),
            AgentTerminalKind::PolicyBudget,
        );
        assert_eq!(
            classify_agent_terminal("verify_exhausted", "done_judge_cap_reached", false, None),
            AgentTerminalKind::PolicyBudget,
        );
        assert_eq!(
            classify_agent_terminal(
                "completion_unverified",
                "done_judge_cap_reached",
                false,
                None,
            ),
            AgentTerminalKind::CompletionUnverified,
        );
        assert_eq!(
            classify_agent_terminal("stuck", "thrash_hard_stop", false, None),
            AgentTerminalKind::PolicyThrash,
        );
        assert_eq!(
            classify_agent_terminal("stuck", "max_nudges", false, None),
            AgentTerminalKind::PolicyNoProgress,
        );
        assert_eq!(
            classify_agent_terminal("input_guardrail", "input_guardrail_tripwire", false, None),
            AgentTerminalKind::PolicyGuardrail,
        );
        assert_eq!(
            classify_agent_terminal("scope_alert", "out_of_scope", false, None),
            AgentTerminalKind::PolicyGuardrail,
        );
    }

    #[test]
    fn errors_split_provider_vs_harness_by_class() {
        assert_eq!(
            classify_agent_terminal("provider_error", "escalation_aborted", true, None),
            AgentTerminalKind::ProviderError,
        );
        for class in AgentTerminalClass::ALL
            .into_iter()
            .filter(|class| class.is_provider_error())
        {
            assert_eq!(
                classify_agent_terminal_with_class("error", "boom", true, Some(class)),
                AgentTerminalKind::ProviderError,
                "class {class} should attribute to the provider"
            );
        }
        for class in AgentTerminalClass::ALL
            .into_iter()
            .filter(|class| !class.is_provider_error())
        {
            assert_eq!(
                classify_agent_terminal_with_class("error", "boom", true, Some(class)),
                AgentTerminalKind::RuntimeError,
                "class {class} should attribute to the harness"
            );
        }
        // A `done` status that nonetheless carries a terminal error is an error,
        // classified by its class rather than reported as a completion.
        assert_eq!(
            classify_agent_terminal_with_class(
                "done",
                "completed",
                true,
                Some(AgentTerminalClass::ProviderMisconfigured),
            ),
            AgentTerminalKind::ProviderError,
        );
    }

    #[test]
    fn cancel_and_block_statuses_classify_by_owner() {
        for reason in ["cancelled", "canceled", "aborted"] {
            assert_eq!(
                classify_agent_terminal(reason, "user_cancel", false, None),
                AgentTerminalKind::UserCancelled,
                "status {reason} should attribute to the user"
            );
        }
        assert_eq!(
            classify_agent_terminal("blocked", "user_prompt_submit_blocked", false, None),
            AgentTerminalKind::PolicyGuardrail,
        );
    }

    #[test]
    fn suspended_and_unknown() {
        assert_eq!(
            classify_agent_terminal("suspended", "suspended", false, None),
            AgentTerminalKind::Suspended,
        );
        assert_eq!(
            classify_agent_terminal("some_future_status", "whatever", false, None),
            AgentTerminalKind::Unknown,
        );
    }

    #[test]
    fn the_finalize_seam_carries_the_class_it_was_given() {
        // The defect this closes: the class was computed at the finalize
        // boundary, used only to split provider from harness, and then dropped,
        // so a consumer reading the outcome could say "the provider failed" but
        // never "no credential resolved for the selected provider".
        let outcome = terminal_outcome_for_finalize(
            "error",
            "exception",
            Some(AgentTerminalClass::ProviderMisconfigured),
            true,
        );

        assert_eq!(outcome.kind, AgentTerminalKind::ProviderError);
        assert_eq!(outcome.reason, "exception");
        assert_eq!(outcome.owner, "provider");
        assert_eq!(
            outcome.terminal_class,
            Some(AgentTerminalClass::ProviderMisconfigured),
            "the class the boundary computed must reach the consumer"
        );
    }

    #[test]
    fn the_finalize_seam_falls_back_to_the_status_when_there_is_no_stop_reason() {
        let outcome = terminal_outcome_for_finalize("suspended", "", None, false);

        assert_eq!(outcome.kind, AgentTerminalKind::Suspended);
        assert_eq!(outcome.reason, "suspended");
        assert_eq!(outcome.terminal_class, None);
    }

    #[test]
    fn the_finalize_seam_reports_a_natural_completion_with_nothing_attached() {
        let outcome = terminal_outcome_for_finalize("done", "completed", None, false);

        assert_eq!(outcome.kind, AgentTerminalKind::Natural);
        assert_eq!(outcome.owner, "agent");
        assert_eq!(outcome.terminal_class, None);
    }

    #[test]
    fn a_class_rides_along_with_the_outcome_that_carries_it() {
        let outcome = AgentTerminalOutcome::new(AgentTerminalKind::ProviderError, "exception")
            .with_terminal_class(Some(AgentTerminalClass::ProviderMisconfigured));

        assert_eq!(
            outcome.terminal_class,
            Some(AgentTerminalClass::ProviderMisconfigured)
        );
        // The kind says who is responsible; the class says what went wrong. A
        // consumer needs both to tell an unresolvable credential from a rate
        // limit, and the key matches the failed-prompt frame's.
        assert_eq!(outcome.to_json()["terminalClass"], "provider_misconfigured");
        assert_eq!(outcome.to_json()["kind"], "provider_error");
        assert_eq!(outcome.to_json()["owner"], "provider");
    }

    #[test]
    fn an_outcome_without_a_class_serializes_exactly_as_it_did_before() {
        let outcome = AgentTerminalOutcome::new(AgentTerminalKind::PolicyBudget, "max_iterations");

        assert_eq!(outcome.terminal_class, None);
        assert_eq!(
            outcome.to_json(),
            serde_json::json!({
                "kind": "policy_budget",
                "reason": "max_iterations",
                "owner": "policy",
            }),
            "the key must be absent, not null, so an existing consumer sees identical bytes"
        );
        let encoded = serde_json::to_value(&outcome).unwrap();
        assert!(encoded.get("terminalClass").is_none());
    }

    #[test]
    fn the_class_round_trips_and_an_older_payload_without_it_still_decodes() {
        let outcome = AgentTerminalOutcome::new(AgentTerminalKind::ProviderError, "error")
            .with_terminal_class(Some(AgentTerminalClass::RateLimited));
        let encoded = serde_json::to_value(&outcome).unwrap();
        assert_eq!(encoded["terminalClass"], "rate_limited");
        let decoded: AgentTerminalOutcome = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, outcome);

        // A producer that predates this field must keep decoding.
        let legacy: AgentTerminalOutcome = serde_json::from_value(serde_json::json!({
            "kind": "unknown",
            "reason": "exception",
            "owner": "unknown",
        }))
        .expect("a payload without the field still decodes");
        assert_eq!(legacy.terminal_class, None);
    }

    #[test]
    fn an_unrecognized_status_still_attributes_a_classified_error() {
        // The reported symptom: a turn that died on a missing provider
        // credential reached its embedder as kind `unknown`, reason
        // `exception`, because the status the pipeline sealed was not in the
        // table above — even though the finalize host had already classified
        // the failure as `provider_misconfigured`.
        assert_eq!(
            classify_agent_terminal(
                "exception",
                "exception",
                true,
                Some("provider_misconfigured"),
            ),
            AgentTerminalKind::ProviderError,
        );
        assert_eq!(
            classify_agent_terminal("exception", "exception", true, Some("generic_throw")),
            AgentTerminalKind::RuntimeError,
        );
        // An error with no class at all is still owned by the harness rather
        // than left unattributed.
        assert_eq!(
            classify_agent_terminal("exception", "exception", true, None),
            AgentTerminalKind::RuntimeError,
        );
    }

    #[test]
    fn unknown_stays_reserved_for_terminal_conditions_with_nothing_to_attribute() {
        for status in ["some_future_status", "exception", "weird"] {
            assert_eq!(
                classify_agent_terminal(status, "whatever", false, None),
                AgentTerminalKind::Unknown,
                "status {status} with no error and no class is genuinely unknown"
            );
        }
    }

    #[test]
    fn every_terminal_class_attributes_an_unrecognized_status_to_a_real_owner() {
        for class in AgentTerminalClass::ALL {
            let kind =
                classify_agent_terminal_with_class("exception", "exception", true, Some(class));
            assert_ne!(
                kind,
                AgentTerminalKind::Unknown,
                "{class:?} carries enough to attribute an owner"
            );
            assert_eq!(
                kind == AgentTerminalKind::ProviderError,
                class.is_provider_error(),
                "{class:?} must split provider from harness the same way a known status does"
            );
        }
    }

    #[test]
    fn outcome_carries_reason_and_derives_owner() {
        let outcome = AgentTerminalOutcome::new(AgentTerminalKind::PolicyBudget, "max_iterations");
        assert_eq!(outcome.kind, AgentTerminalKind::PolicyBudget);
        assert_eq!(outcome.reason, "max_iterations");
        assert_eq!(outcome.owner, "policy");
        let json = outcome.to_json();
        assert_eq!(json["kind"], "policy_budget");
        assert_eq!(json["reason"], "max_iterations");
        assert_eq!(json["owner"], "policy");
    }
}
