//! The seam that gives an `Ask` somebody to ask.
//!
//! # Why this exists
//!
//! A run with no person attached used to have exactly one answer to "this tool
//! call needs approval": refuse it. On a headless or eval host the approval
//! bridge is absent, so every `Ask` fell out at `DenialGate::ApprovalUnavailable`
//! with "approval required but no host bridge is available". That is a correct
//! refusal and a useless one — the work stops for want of a decision nobody was
//! there to make.
//!
//! [`ApprovalResolver`](super::ApprovalResolver) names *who answers*, orthogonal
//! to what the policy decided. This module holds the `AutoReview` answerer: a
//! Harn closure the embedder installs, which runs a separate reviewer session
//! and returns a verdict.
//!
//! # Fail closed, and why that is the opposite of the precheck
//!
//! [`tool_precheck`](super::tool_precheck) is deliberately fail-OPEN: it can
//! only ever *add* a refusal, so a broken precheck leaves dispatch exactly as
//! permissive as it was. This seam can only ever *lift* a refusal, so the same
//! choice would be a security hole. Every ambiguity here resolves to "not
//! approved":
//!
//! - no reviewer installed
//! - no VM context to run it in
//! - the closure raised
//! - the verdict is unparseable, or does not say `approved`
//! - the seam is re-entrant (a reviewer's own tool calls are never reviewed)
//!
//! An unreviewed call is denied exactly as it would have been without this
//! module. The seam can rescue work; it can never weaken a refusal it failed to
//! understand.
//!
//! # The breaker
//!
//! A reviewer that denies over and over is either facing a model that will not
//! take no for an answer or is itself broken. Both are worth stopping rather
//! than paying for. Counts are per session and read by the dispatch site.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::value::{VmClosure, VmError, VmValue};

/// No reviewer was installed to reconsider a refusal.
///
/// One owner for the string because two seams record it: the review runner,
/// which reports it as an outcome, and the grant seam, which stamps it onto the
/// decision receipt without running a review at all.
pub const NO_REVIEWER_INSTALLED: &str = "no_reviewer_installed";

mod evaluation;
pub use evaluation::{DecisionReview, ReviewDisposition};
#[cfg(test)]
mod evaluation_tests;

thread_local! {
    static APPROVAL_REVIEWER_STACK: RefCell<Vec<Arc<VmClosure>>> = const { RefCell::new(Vec::new()) };
    /// Re-entrancy depth. The reviewer session dispatches its own tool calls;
    /// reviewing those would recurse without bound. The seam is inert while a
    /// review is in flight, which fails closed for the nested call.
    static APPROVAL_REVIEWER_DEPTH: RefCell<usize> = const { RefCell::new(0) };
    /// Per-session breaker counters: (consecutive denials, denials this turn).
    static APPROVAL_REVIEWER_DENIALS: RefCell<HashMap<String, BreakerCounts>> =
        RefCell::new(HashMap::new());
}

/// Consecutive reviewer denials that trip the breaker.
pub const CONSECUTIVE_DENIAL_LIMIT: u32 = 3;
/// Reviewer denials within one turn that trip the breaker.
pub const PER_TURN_DENIAL_LIMIT: u32 = 10;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BreakerCounts {
    pub consecutive: u32,
    pub this_turn: u32,
}

impl BreakerCounts {
    /// Whether the reviewer has denied enough to stop asking it.
    ///
    /// Two independent limits, because they catch different failures: a run of
    /// consecutive denials means the model is not taking the hint, while a high
    /// total across a turn means the reviewer is refusing broadly even if the
    /// model occasionally gets through.
    pub fn tripped(self) -> bool {
        self.consecutive >= CONSECUTIVE_DENIAL_LIMIT || self.this_turn >= PER_TURN_DENIAL_LIMIT
    }
}

/// A reviewer's answer, normalized once at this boundary.
///
/// `approved` is the only field the dispatch site acts on. The rest is evidence
/// and rides into the activity record so a run can be audited later.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApprovalReviewOutcome {
    pub approved: bool,
    /// Validated evaluator outcome and the deterministic rule applied to it.
    pub evaluation_review: Option<Box<DecisionReview>>,
    /// Whether a reviewer actually answered. `false` distinguishes "the
    /// reviewer considered this and said no" from "nothing answered, so it
    /// stays refused" — those are the same denial with very different meanings,
    /// and collapsing them would make a broken reviewer look like a strict one.
    pub reviewer_answered: bool,
    pub rationale: String,
    pub risk: Option<String>,
    pub authorization: Option<String>,
    /// Why no conclusive review was applied, including an uncertain candidate.
    pub unavailable_reason: Option<String>,
}

impl ApprovalReviewOutcome {
    /// The refusal used whenever the seam could not obtain a verdict.
    fn unavailable(reason: &str) -> Self {
        Self {
            approved: false,
            evaluation_review: None,
            reviewer_answered: false,
            rationale: String::new(),
            risk: None,
            authorization: None,
            unavailable_reason: Some(reason.to_string()),
        }
    }
}

pub fn push_approval_reviewer(reviewer: Arc<VmClosure>) {
    APPROVAL_REVIEWER_STACK.with(|stack| stack.borrow_mut().push(reviewer));
}

pub fn pop_approval_reviewer() {
    APPROVAL_REVIEWER_STACK.with(|stack| {
        stack.borrow_mut().pop();
    });
}

pub fn clear_approval_reviewers() {
    APPROVAL_REVIEWER_STACK.with(|stack| stack.borrow_mut().clear());
    APPROVAL_REVIEWER_DEPTH.with(|depth| *depth.borrow_mut() = 0);
    APPROVAL_REVIEWER_DENIALS.with(|counts| counts.borrow_mut().clear());
}

pub fn current_approval_reviewer() -> Option<Arc<VmClosure>> {
    APPROVAL_REVIEWER_STACK.with(|stack| stack.borrow().last().cloned())
}

/// Whether a reviewer is installed. The dispatch fast path reads this so a run
/// without one keeps its exact previous behavior.
pub fn approval_reviewer_active() -> bool {
    APPROVAL_REVIEWER_STACK.with(|stack| !stack.borrow().is_empty())
}

pub(crate) fn swap_approval_reviewer_stack(next: Vec<Arc<VmClosure>>) -> Vec<Arc<VmClosure>> {
    APPROVAL_REVIEWER_STACK.with(|stack| std::mem::replace(&mut *stack.borrow_mut(), next))
}

pub(crate) fn swap_approval_reviewer_depth(next: usize) -> usize {
    APPROVAL_REVIEWER_DEPTH.with(|depth| std::mem::replace(&mut *depth.borrow_mut(), next))
}

/// Current breaker counts for a session. A session that has never been reviewed
/// reads as all-zero, which is a true zero: no reviews, so no denials.
pub fn approval_reviewer_breaker(session_id: &str) -> BreakerCounts {
    APPROVAL_REVIEWER_DENIALS
        .with(|counts| counts.borrow().get(session_id).copied())
        .unwrap_or_default()
}

/// Reset the per-turn denial count at a turn boundary. The consecutive count
/// deliberately survives: a reviewer denying the same thing across a turn
/// boundary is exactly the loop the breaker exists to catch.
pub fn reset_approval_reviewer_turn(session_id: &str) {
    APPROVAL_REVIEWER_DENIALS.with(|counts| {
        if let Some(entry) = counts.borrow_mut().get_mut(session_id) {
            entry.this_turn = 0;
        }
    });
}

fn record_verdict(session_id: &str, approved: bool) {
    APPROVAL_REVIEWER_DENIALS.with(|counts| {
        let mut counts = counts.borrow_mut();
        let entry = counts.entry(session_id.to_string()).or_default();
        if approved {
            entry.consecutive = 0;
        } else {
            entry.consecutive = entry.consecutive.saturating_add(1);
            entry.this_turn = entry.this_turn.saturating_add(1);
        }
    });
}

struct DepthGuard;

impl Drop for DepthGuard {
    fn drop(&mut self) {
        APPROVAL_REVIEWER_DEPTH.with(|depth| {
            let mut depth = depth.borrow_mut();
            *depth = depth.saturating_sub(1);
        });
    }
}

/// Ask the installed reviewer whether one refused call may proceed.
///
/// Returns an outcome in every case, never an error: a reviewer that fails is a
/// refusal, not a crashed run. The caller acts on `approved` alone.
pub async fn run_approval_review(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    request: serde_json::Value,
    session_id: &str,
) -> ApprovalReviewOutcome {
    let Some(reviewer) = current_approval_reviewer() else {
        return ApprovalReviewOutcome::unavailable(NO_REVIEWER_INSTALLED);
    };
    if APPROVAL_REVIEWER_DEPTH.with(|depth| *depth.borrow()) > 0 {
        return ApprovalReviewOutcome::unavailable("reviewer_reentrant");
    }
    if approval_reviewer_breaker(session_id).tripped() {
        return ApprovalReviewOutcome::unavailable("breaker_tripped");
    }
    let Some(mut vm) = ctx.map(crate::vm::AsyncBuiltinCtx::child_vm) else {
        return ApprovalReviewOutcome::unavailable("no_vm_context");
    };
    let arg = crate::stdlib::json_to_vm_value(&request);
    APPROVAL_REVIEWER_DEPTH.with(|depth| *depth.borrow_mut() += 1);
    let _guard = DepthGuard;
    let outcome = match vm.call_closure_pub(&reviewer, &[arg]).await {
        Ok(value) => parse_review_verdict(value),
        // A raising reviewer is a refusal. Propagating the error would abort a
        // run over a failed *optional* rescue attempt, turning a recoverable
        // denial into a dead run.
        Err(VmError::Runtime(message)) => {
            ApprovalReviewOutcome::unavailable(&format!("reviewer_error: {message}"))
        }
        Err(_) => ApprovalReviewOutcome::unavailable("reviewer_error"),
    };
    if outcome.reviewer_answered {
        record_verdict(session_id, outcome.approved);
    }
    outcome
}

/// Normalize a reviewer closure's return.
///
/// Fail CLOSED, unlike the precheck's parser: anything this cannot read as an
/// explicit approval is not an approval. In particular a bare `true` is NOT
/// accepted — the reviewer contract returns a decision record, and treating a
/// stray truthy value as a grant is how a seam like this quietly stops meaning
/// anything.
fn parse_review_verdict(value: VmValue) -> ApprovalReviewOutcome {
    let VmValue::Dict(map) = value else {
        return ApprovalReviewOutcome::unavailable("reviewer_unparseable");
    };
    let approved = matches!(map.get("approved"), Some(VmValue::Bool(true)));
    if let Some(value) = map.get("evaluation_review") {
        let review = match DecisionReview::parse(value) {
            Ok(review) => review,
            Err(reason) => return ApprovalReviewOutcome::unavailable(reason),
        };
        if approved != (review.disposition == ReviewDisposition::Approved) {
            return ApprovalReviewOutcome::unavailable("inconsistent_review_disposition");
        }
        let reviewer_answered = review.disposition != ReviewDisposition::NeedsReview;
        let unavailable_reason = (!reviewer_answered).then(|| {
            if review.outcome_kind() == "answered" {
                review.rule.clone()
            } else {
                review.outcome_kind().to_string()
            }
        });
        return ApprovalReviewOutcome {
            approved,
            evaluation_review: Some(Box::new(review)),
            reviewer_answered,
            rationale: string_field(&map, "rationale").unwrap_or_default(),
            risk: string_field(&map, "risk"),
            authorization: string_field(&map, "authorization"),
            unavailable_reason,
        };
    }
    // `reviewer_answered` comes from the record when present. Absent, a
    // well-formed decision (one that named an outcome) still counts as an
    // answer, so a reviewer that omits the field is not mistaken for a broken
    // one.
    let answered = match map.get("reviewer_answered") {
        Some(VmValue::Bool(flag)) => *flag,
        _ => map.get("approved").is_some() || map.get("outcome").is_some(),
    };
    if !answered {
        let reason = string_field(&map, "unavailable_reason")
            .unwrap_or_else(|| "reviewer_did_not_answer".to_string());
        return ApprovalReviewOutcome::unavailable(&reason);
    }
    ApprovalReviewOutcome {
        approved,
        evaluation_review: None,
        reviewer_answered: true,
        rationale: string_field(&map, "rationale").unwrap_or_default(),
        risk: string_field(&map, "risk"),
        authorization: string_field(&map, "authorization"),
        unavailable_reason: None,
    }
}

fn string_field(map: &crate::value::DictMap, key: &str) -> Option<String> {
    match map.get(key) {
        Some(VmValue::String(text)) if !text.is_empty() => Some(text.to_string()),
        _ => None,
    }
}

/// Extract the reviewer closure from an agent-loop `approval_reviewer` option.
pub fn parse_approval_reviewer_value(
    value: Option<&VmValue>,
    label: &str,
) -> Result<Option<Arc<VmClosure>>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Closure(closure)) => Ok(Some(closure.clone())),
        Some(other) => Err(VmError::Runtime(format!(
            "{label} must be a closure, got {}",
            other.type_name()
        ))),
    }
}

/// Whether the automated reviewer is what turned this decision into an allow.
///
/// Reads the receipt `grant_by_auto_review` writes rather than a flag carried
/// beside it, so what a caller branches on is what an auditor reads back. A
/// separate flag could disagree with the receipt; this cannot.
pub fn granted_by_auto_review(decision: &crate::orchestration::PolicyEvaluation) -> bool {
    decision
        .receipt
        .get("auto_review")
        .is_some_and(serde_json::Value::is_object)
}

/// Whether an allowed dispatch has a grant worth recording.
///
/// `has_audit_signal` alone was the old condition, and it misses a reviewer
/// grant on an ask rule carrying neither a rule id nor a risk label. That
/// combination left no record at all of a call a reviewer let through, which
/// is the one case where the record matters most.
pub fn decision_records_a_grant(decision: &crate::orchestration::PolicyEvaluation) -> bool {
    decision.has_audit_signal() || granted_by_auto_review(decision)
}

/// A side-effect review retains evidence even when it cannot grant the call.
#[derive(Default)]
pub struct SideEffectApprovalReview {
    pub grant: Option<serde_json::Value>,
    pub evaluation_review: Option<Box<DecisionReview>>,
}

/// Offer one side-effect-ceiling refusal to the reviewer.
///
/// Returns a grant or decision evidence for the existing human fallback.
///
/// A ceiling refusal is a static policy refusal that the dispatch site may
/// already offer to a person, so it is exactly the kind of refusal this seam
/// exists for. It was not offered here, which meant a run carrying both a
/// capability policy and a reviewer refused the call and never asked
/// (harn#7982). That is the shape every product loop sends, so the reviewer
/// looked installed everywhere and answered nowhere.
///
/// Lives beside `maybe_grant_by_auto_review` because when a refusal may be
/// reconsidered is this modules decision, not the dispatch sites.
pub async fn maybe_grant_side_effect_by_auto_review(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    tool_name: &str,
    tool_args: &serde_json::Value,
    session_id: &str,
    ceiling: &str,
    required_level: &str,
    reason: &str,
) -> SideEffectApprovalReview {
    if !approval_reviewer_active() {
        return SideEffectApprovalReview::default();
    }
    let policy_decision = serde_json::json!({
        "action": "ask",
        "source": "side_effect_ceiling",
        "scope": "once",
        "ceiling": ceiling,
        "required_level": required_level,
        "tool": tool_name,
    });
    let request = serde_json::json!({
        "tool": tool_name,
        "arguments": tool_args,
        "session_id": session_id,
        "action": "ask",
        "reason": reason,
        "risk_labels": ["side_effect_ceiling"],
        "policy_decision": policy_decision,
    });
    let outcome = run_approval_review(ctx, request, session_id).await;
    if !outcome.approved {
        return SideEffectApprovalReview {
            grant: None,
            evaluation_review: outcome.evaluation_review,
        };
    }
    let mut granted = policy_decision;
    granted["action"] = serde_json::Value::String("allow".to_string());
    granted["granted_by"] = serde_json::Value::String("approval_reviewer".to_string());
    granted["rationale"] = serde_json::Value::String(outcome.rationale);
    if let Some(review) = outcome.evaluation_review {
        granted["auto_review"] = serde_json::json!({
            "approved": true,
            "reviewer_answered": true,
            "evaluation_review": review,
        });
    }
    SideEffectApprovalReview {
        grant: Some(granted),
        evaluation_review: None,
    }
}

/// Record that this seam was reached for a refusal and did NOT grant it.
///
/// The counterpart to [`PolicyEvaluation::grant_by_auto_review`], which lives
/// on the decision because a grant rewrites it. A decline changes nothing about
/// the decision and only annotates its receipt, so it belongs to the seam that
/// declined rather than to the policy type.
///
/// `reviewer_answered` separates the two cases a reader must not confuse:
/// `true` means a reviewer ran and said no, `false` means no verdict was
/// obtained and `unavailable_reason` says which decline path fired.
fn record_decline(
    decision: &mut crate::orchestration::PolicyEvaluation,
    reviewer_answered: bool,
    rationale: &str,
    unavailable_reason: Option<&str>,
) {
    if let Some(map) = decision.receipt.as_object_mut() {
        map.insert(
            "auto_review".to_string(),
            serde_json::json!({
                "approved": false,
                "reviewer_answered": reviewer_answered,
                "rationale": rationale,
                "unavailable_reason": unavailable_reason,
                "decider": "auto_reviewer",
            }),
        );
    }
}

/// Whether a reviewer actually answered this refusal and said no.
///
/// Reads the annotation [`record_decline`] leaves on the receipt. A caller uses
/// it to attribute the refusal to the layer that truly decided it: when the
/// reviewer answered, a later host refusal is a formality and recording the
/// host's default decider credits whoever the host names for a call the
/// reviewer had already settled.
///
/// False for every decline the reviewer did not answer, so a seam that could
/// not obtain a verdict never reads as one that refused.
pub fn reviewer_refused(decision: &crate::orchestration::PolicyEvaluation) -> bool {
    decision
        .receipt
        .get("auto_review")
        .and_then(|entry| {
            let answered = entry.get("reviewer_answered")?.as_bool()?;
            let approved = entry.get("approved")?.as_bool()?;
            Some(answered && !approved)
        })
        .unwrap_or(false)
}

/// Offer one refused decision to the reviewer, rewriting it in place on a grant.
///
/// Returns whether the decision was granted, so the caller can record the
/// approval status. Lives here rather than at the dispatch site because the
/// decision of *when* a refusal may be reconsidered belongs to this module, and
/// because the dispatch function is under a file-length ratchet that a body
/// this size would push past its ceiling.
///
/// Only `Ask` and `Deny` are offered. An `Allow` is not a refusal and there is
/// nothing to reconsider; passing one here would let the seam widen a decision
/// nobody had objected to.
pub async fn maybe_grant_by_auto_review(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    decision: Option<&mut crate::orchestration::PolicyEvaluation>,
    tool_name: &str,
    tool_args: &serde_json::Value,
    session_id: &str,
) -> bool {
    let Some(decision) = decision else {
        return false;
    };
    // Only a refusal is offered; an `Allow` is not this seam's business and
    // must not be annotated as though a reviewer had weighed in on it.
    if !decision.is_deny() && !decision.is_ask() {
        return false;
    }
    // A reviewer that is not installed is recorded, not skipped. This used to
    // return here in silence, which is what made the two runs that matter
    // indistinguishable: one whose reviewer refused, and one whose reviewer was
    // never installed at all. The second then reached the host, and a host with
    // nobody to ask produced a bare `host_rejected` -- a record naming the last
    // layer to say no rather than the layer that was supposed to answer.
    if !approval_reviewer_active() {
        record_decline(decision, false, "", Some(NO_REVIEWER_INSTALLED));
        return false;
    }
    let request = serde_json::json!({
        "tool": tool_name,
        "arguments": tool_args,
        "session_id": session_id,
        "action": decision.action,
        "reason": decision.reason,
        "risk_labels": decision.risk_labels,
        "policy_decision": decision.receipt,
    });
    let outcome = run_approval_review(ctx, request, session_id).await;
    if !outcome.approved {
        // Either a reviewer answered and said no, or the seam could not obtain
        // a verdict and `unavailable_reason` says which of its decline paths
        // fired. Both are recorded; neither is inferred from the absence of a
        // grant.
        record_decline(
            decision,
            outcome.reviewer_answered,
            &outcome.rationale,
            outcome.unavailable_reason.as_deref(),
        );
        attach_evaluation_review(decision, outcome.evaluation_review.as_deref());
        return false;
    }
    decision.grant_by_auto_review(&outcome.rationale);
    attach_evaluation_review(decision, outcome.evaluation_review.as_deref());
    true
}

fn attach_evaluation_review(
    decision: &mut crate::orchestration::PolicyEvaluation,
    review: Option<&DecisionReview>,
) {
    if let (Some(review), Some(auto_review)) = (
        review,
        decision
            .receipt
            .get_mut("auto_review")
            .and_then(serde_json::Value::as_object_mut),
    ) {
        auto_review.insert("evaluation_review".to_string(), serde_json::json!(review));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
        let mut map = crate::value::DictMap::default();
        for (key, value) in pairs {
            map.insert(arcstr::ArcStr::from(*key), value.clone());
        }
        VmValue::Dict(std::sync::Arc::new(map))
    }

    fn text(value: &str) -> VmValue {
        VmValue::String(arcstr::ArcStr::from(value))
    }

    #[test]
    fn an_explicit_approval_is_an_approval() {
        let outcome = parse_review_verdict(dict(&[
            ("approved", VmValue::Bool(true)),
            ("rationale", text("the goal names this file")),
        ]));
        assert!(outcome.approved);
        assert!(outcome.reviewer_answered);
        assert_eq!(outcome.rationale, "the goal names this file");
    }

    #[test]
    fn a_bare_true_is_not_an_approval() {
        // The falsifier for the fail-closed claim. If this seam ever accepts a
        // truthy non-record, every buggy reviewer becomes a rubber stamp.
        let outcome = parse_review_verdict(VmValue::Bool(true));
        assert!(!outcome.approved);
        assert!(!outcome.reviewer_answered);
        assert_eq!(
            outcome.unavailable_reason.as_deref(),
            Some("reviewer_unparseable")
        );
    }

    #[test]
    fn a_missing_approved_field_is_not_an_approval() {
        let outcome = parse_review_verdict(dict(&[("rationale", text("looks fine"))]));
        assert!(!outcome.approved);
    }

    #[test]
    fn an_answered_denial_is_not_an_unavailable() {
        // These are the same refusal and must not read the same: one is a
        // judgment, the other is a broken instrument.
        let outcome = parse_review_verdict(dict(&[
            ("approved", VmValue::Bool(false)),
            ("rationale", text("unrelated to the goal")),
        ]));
        assert!(!outcome.approved);
        assert!(outcome.reviewer_answered);
        assert!(outcome.unavailable_reason.is_none());
    }

    #[test]
    fn the_breaker_trips_on_consecutive_denials() {
        let mut counts = BreakerCounts::default();
        for _ in 0..CONSECUTIVE_DENIAL_LIMIT {
            assert!(!counts.tripped());
            counts.consecutive += 1;
            counts.this_turn += 1;
        }
        assert!(counts.tripped());
    }

    #[test]
    fn an_approval_clears_the_consecutive_run_but_not_the_turn() {
        clear_approval_reviewers();
        let session = "s-breaker";
        record_verdict(session, false);
        record_verdict(session, false);
        assert_eq!(approval_reviewer_breaker(session).consecutive, 2);
        record_verdict(session, true);
        let counts = approval_reviewer_breaker(session);
        assert_eq!(counts.consecutive, 0, "an approval breaks the run");
        assert_eq!(
            counts.this_turn, 2,
            "but the turn total still remembers both denials"
        );
        clear_approval_reviewers();
    }

    #[test]
    fn a_turn_reset_leaves_the_consecutive_run_standing() {
        clear_approval_reviewers();
        let session = "s-turn";
        record_verdict(session, false);
        record_verdict(session, false);
        reset_approval_reviewer_turn(session);
        let counts = approval_reviewer_breaker(session);
        assert_eq!(counts.this_turn, 0);
        assert_eq!(
            counts.consecutive, 2,
            "a reviewer denying across a turn boundary is the loop we are catching"
        );
        clear_approval_reviewers();
    }

    #[test]
    fn an_unreviewed_session_reads_a_true_zero() {
        clear_approval_reviewers();
        let counts = approval_reviewer_breaker("never-seen");
        assert_eq!(counts.consecutive, 0);
        assert!(!counts.tripped());
    }
}
