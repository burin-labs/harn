//! The budgeted decision evaluator.
//!
//! One evaluation answers one question set over one state, makes at most one
//! physical request, and returns a closed outcome with a receipt. Everything
//! that can be decided locally is decided before dispatch, so a refusal the
//! route's own limits imply costs nothing: the `state_too_large` and
//! `question_invalid` arms are reached with a provider request count of zero.
//!
//! Both entry points run this same code. `harness.llm.evaluate_predicate` is
//! the single-boolean projection of `harness.llm.evaluate`, projected at the
//! end rather than implemented twice, so the two cannot refuse differently.

pub(crate) mod answer;
pub(crate) mod backend;
pub(crate) mod contract;
#[cfg(test)]
pub(crate) mod mock;
pub(crate) mod native;
pub(crate) mod outcome;
pub(crate) mod question;
pub mod receipt;
pub(crate) mod structured;
pub(crate) mod transport;

#[cfg(test)]
mod tests;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use answer::Answer;
use backend::{DecisionBackend, DecisionRequest, DecisionTransportError, RefusalReason};
use contract::DecisionContract;
use outcome::Outcome;
use question::QuestionSet;
use receipt::{AccountingStatus, EvaluationIdentity, EvaluationReceipt, EvaluationSource};

use crate::value::{VmError, VmValue};

/// The admitted policy. Built once, before anything dispatches, so a malformed
/// policy cannot reach a provider.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EvaluationPolicy {
    pub backend: BackendKind,
    pub provider: String,
    pub model: String,
    pub effort: String,
    pub temperature: f64,
    pub native_options_supplied: bool,
    pub threshold: f64,
    pub evaluation_cost_limit: Option<f64>,
    pub run_cost_limit: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BackendKind {
    StructuredLlm,
    NativeDecision,
}

impl BackendKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::StructuredLlm => "structured_llm",
            Self::NativeDecision => "native_decision",
        }
    }
}

impl EvaluationPolicy {
    fn from_value(value: &VmValue) -> Result<Self, String> {
        let fields = value
            .as_dict()
            .ok_or_else(|| "evaluation policy must be a record".to_string())?;
        let harn_builtin_meta::Ty::Shape(allowed) = harn_builtin_meta::predicate::POLICY else {
            unreachable!("evaluation policy is a closed record");
        };
        if let Some(key) = fields
            .keys()
            .find(|key| !allowed.iter().any(|field| field.name == key.as_str()))
        {
            return Err(format!("evaluation policy has unknown field `{key}`"));
        }
        let text = |key: &str| match fields.get(key) {
            Some(VmValue::String(value)) => Ok(value.to_string()),
            _ => Err(format!("evaluation policy has no string `{key}`")),
        };
        let number = |key: &str| match fields.get(key) {
            Some(VmValue::Float(value)) => Ok(*value),
            Some(VmValue::Int(value)) => Ok(*value as f64),
            _ => Err(format!("evaluation policy has no numeric `{key}`")),
        };
        let backend = match text("backend")?.as_str() {
            "structured_llm" => BackendKind::StructuredLlm,
            "native_decision" => BackendKind::NativeDecision,
            other => return Err(format!("unknown evaluation backend `{other}`")),
        };
        let policy = Self {
            backend,
            provider: text("provider")?,
            model: text("model")?,
            effort: if fields.contains_key("effort") {
                text("effort")?
            } else {
                "none".into()
            },
            temperature: if fields.contains_key("temperature") {
                number("temperature")?
            } else {
                0.0
            },
            native_options_supplied: fields.contains_key("effort")
                || fields.contains_key("temperature"),
            threshold: number("threshold")?,
            evaluation_cost_limit: fields
                .contains_key("evaluation_cost_limit")
                .then(|| number("evaluation_cost_limit"))
                .transpose()?,
            run_cost_limit: fields
                .contains_key("run_cost_limit")
                .then(|| number("run_cost_limit"))
                .transpose()?,
        };
        if !policy.threshold.is_finite()
            || !(0.0..=1.0).contains(&policy.threshold)
            || !policy.temperature.is_finite()
            || policy
                .evaluation_cost_limit
                .is_some_and(|limit| !limit.is_finite() || limit < 0.0)
            || policy
                .run_cost_limit
                .is_some_and(|limit| !limit.is_finite() || limit < 0.0)
        {
            return Err(
                "evaluation policy requires finite nonnegative budgets and a threshold in [0, 1]"
                    .into(),
            );
        }
        Ok(policy)
    }

    /// A digest of every policy fact that changes what was asked. The
    /// threshold is included: the same answers under a different threshold are
    /// a different decision, and a cache that ignored it would silently reuse
    /// an acceptance the caller no longer wants.
    fn digest(&self) -> String {
        let parts = [
            self.backend.as_str().to_string(),
            self.provider.clone(),
            self.model.clone(),
            self.effort.clone(),
            format!("{:?}", self.temperature),
            self.native_options_supplied.to_string(),
            format!("{:?}", self.threshold),
            format!("{:?}", self.evaluation_cost_limit),
            format!("{:?}", self.run_cost_limit),
        ];
        digest_parts(parts.iter().map(String::as_str))
    }
}

/// Length-delimited so two different field splits cannot produce one digest.
fn digest_parts<'a>(parts: impl Iterator<Item = &'a str>) -> String {
    let mut encoded = Vec::new();
    for part in parts {
        encoded.extend_from_slice(&(part.len() as u64).to_le_bytes());
        encoded.extend_from_slice(part.as_bytes());
    }
    format!("blake3:{}", blake3::hash(&encoded).to_hex())
}

fn question_set_digest(questions: &QuestionSet) -> String {
    let value = serde_json::to_value(questions).expect("normalized questions are serializable");
    let encoded = crate::canonical_json::to_vec(&value);
    format!("blake3:{}", blake3::hash(&encoded).to_hex())
}

/// Everything the dispatch step needs, admitted before anything dispatches.
/// The site id, the cache identity, and the state estimate are already on the
/// receipt, so they are not carried a second time here.
struct Evaluation {
    state: serde_json::Value,
    questions: QuestionSet,
    policy: EvaluationPolicy,
}

/// A backend installed for the current process, replacing live transport.
/// Tests install one; production leaves it empty and gets the real backend for
/// the policy's declared kind.
mod installed {
    use super::backend::DecisionBackend;
    use std::sync::{Arc, Mutex, OnceLock};

    static INSTALLED: OnceLock<Mutex<Option<Arc<dyn DecisionBackend>>>> = OnceLock::new();

    fn slot() -> &'static Mutex<Option<Arc<dyn DecisionBackend>>> {
        INSTALLED.get_or_init(|| Mutex::new(None))
    }

    pub(super) fn get() -> Option<Arc<dyn DecisionBackend>> {
        slot().lock().expect("decision backend slot").clone()
    }

    /// Install a backend and return the previous one, so a test restores what
    /// it replaced instead of leaking into the next test.
    #[cfg(test)]
    pub(super) fn swap(
        backend: Option<Arc<dyn DecisionBackend>>,
    ) -> Option<Arc<dyn DecisionBackend>> {
        std::mem::replace(&mut *slot().lock().expect("decision backend slot"), backend)
    }
}

/// Install a decision backend for this process. Returns a guard that restores
/// the previous backend on drop.
#[cfg(test)]
pub(crate) fn install_backend(backend: Arc<dyn DecisionBackend>) -> InstalledBackendGuard {
    InstalledBackendGuard {
        previous: installed::swap(Some(backend)),
    }
}

/// A route contract installed for the current process, standing in for the
/// catalog lookup. Tests install one so the evaluator's own behavior is
/// measured against a declared route rather than against whatever rows the
/// shipped catalog happens to carry.
#[cfg(test)]
mod installed_route {
    use super::contract::DecisionContract;
    use std::collections::BTreeMap;
    use std::sync::{Mutex, OnceLock};

    type Table = BTreeMap<(String, String), DecisionContract>;

    static INSTALLED: OnceLock<Mutex<Table>> = OnceLock::new();

    fn slot() -> &'static Mutex<Table> {
        INSTALLED.get_or_init(|| Mutex::new(Table::new()))
    }

    pub(super) fn get(provider: &str, model: &str) -> Option<DecisionContract> {
        slot()
            .lock()
            .expect("decision route slot")
            .get(&(provider.to_string(), model.to_string()))
            .cloned()
    }

    pub(super) fn insert(
        provider: &str,
        model: &str,
        contract: DecisionContract,
    ) -> Option<DecisionContract> {
        slot()
            .lock()
            .expect("decision route slot")
            .insert((provider.to_string(), model.to_string()), contract)
    }

    pub(super) fn restore(provider: &str, model: &str, previous: Option<DecisionContract>) {
        let mut table = slot().lock().expect("decision route slot");
        let key = (provider.to_string(), model.to_string());
        match previous {
            Some(contract) => {
                table.insert(key, contract);
            }
            None => {
                table.remove(&key);
            }
        }
    }
}

/// Install a route contract for this process. Returns a guard that restores
/// what it replaced on drop.
#[cfg(test)]
pub(crate) fn install_route(
    provider: &str,
    model: &str,
    contract: DecisionContract,
) -> InstalledRouteGuard {
    InstalledRouteGuard {
        provider: provider.to_string(),
        model: model.to_string(),
        previous: installed_route::insert(provider, model, contract),
    }
}

#[cfg(test)]
pub(crate) struct InstalledRouteGuard {
    provider: String,
    model: String,
    previous: Option<DecisionContract>,
}

#[cfg(test)]
impl Drop for InstalledRouteGuard {
    fn drop(&mut self) {
        installed_route::restore(&self.provider, &self.model, self.previous.take());
    }
}

/// The route the evaluator will use. Production reads the catalog; a test may
/// install one first, and an uninstalled route still falls through to the
/// catalog so the unconfigured path is the same code.
#[cfg(test)]
fn resolve_route(provider: &str, model: &str) -> Option<DecisionContract> {
    installed_route::get(provider, model)
        .or_else(|| contract::decision_contract_for_route(provider, model))
}

#[cfg(not(test))]
fn resolve_route(provider: &str, model: &str) -> Option<DecisionContract> {
    contract::decision_contract_for_route(provider, model)
}

#[cfg(test)]
pub(crate) struct InstalledBackendGuard {
    previous: Option<Arc<dyn DecisionBackend>>,
}

#[cfg(test)]
impl Drop for InstalledBackendGuard {
    fn drop(&mut self) {
        installed::swap(self.previous.take());
    }
}

/// When a decision started, for the elapsed time its receipt reports.
///
/// Monotonic rather than wall clock. `SystemTime` steps backwards whenever
/// the host clock is adjusted, which is why reading it twice and subtracting
/// had to saturate to hide a negative duration as zero. `Instant` cannot go
/// backwards, so the number a receipt carries is the time that actually
/// passed. This reaches no digest: a receipt's identity is built from the
/// question and the policy, so replays stay reproducible either way.
fn decision_started() -> std::time::Instant {
    std::time::Instant::now()
}

/// Evaluate a question set. The single-boolean entry point calls this too.
pub(crate) async fn evaluate(
    ctx: &crate::vm::AsyncBuiltinCtx,
    args: &[VmValue],
) -> Result<(Outcome, Vec<Answer>, EvaluationPolicy), VmError> {
    let (outcome, answers, policy, _) = evaluate_internal(ctx, args).await?;
    Ok((outcome, answers, policy))
}

/// The complete result of one evaluation, for CLI and host consumers.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct EvaluationResult {
    pub outcome: serde_json::Value,
    pub receipt: EvaluationReceipt,
}

/// Evaluate through the same admission, transport, accounting and receipt path
/// as `harness.llm.evaluate`. Hosts supply their execution context explicitly.
pub async fn evaluate_json(
    ctx: &crate::vm::AsyncBuiltinCtx,
    site_id: &str,
    state: serde_json::Value,
    questions: serde_json::Value,
    policy: serde_json::Value,
) -> Result<EvaluationResult, VmError> {
    let args = [
        VmValue::string(site_id),
        crate::schema::json_to_vm_value(&state),
        crate::schema::json_to_vm_value(&questions),
        crate::schema::json_to_vm_value(&policy),
    ];
    let (outcome, _, _, receipt) = evaluate_internal(ctx, &args).await?;
    Ok(EvaluationResult {
        outcome: crate::llm::helpers::vm_value_to_json(&outcome.into_value()),
        receipt,
    })
}

async fn evaluate_internal(
    ctx: &crate::vm::AsyncBuiltinCtx,
    args: &[VmValue],
) -> Result<(Outcome, Vec<Answer>, EvaluationPolicy, EvaluationReceipt), VmError> {
    let [id, state, questions, policy] = args else {
        return Err(VmError::Runtime(
            "evaluate expects (id, state, questions, policy)".into(),
        ));
    };
    let id = id.as_str_cow().into_owned();
    let policy = EvaluationPolicy::from_value(policy).map_err(VmError::Runtime)?;
    let questions = QuestionSet::from_value(questions).map_err(VmError::Runtime)?;
    let state = crate::llm::helpers::vm_value_to_json(state);
    let started = decision_started();
    let publish = |receipt: &EvaluationReceipt| {
        ctx.record_evaluation_receipt(receipt.clone());
        publish(receipt);
    };

    let route = resolve_route(&policy.provider, &policy.model);
    let canonical_state = crate::canonical_json::to_vec(&state);
    let identity = EvaluationIdentity {
        structured_output_strategy: route
            .as_ref()
            .and_then(|route| route.structured_output_strategy)
            .map(|strategy| strategy.as_str().to_string()),
        input_digest: digest_parts(std::iter::once(
            std::str::from_utf8(&canonical_state).unwrap_or(""),
        )),
        canonical_input_type: state_type_name(&state).into(),
        question_set_digest: question_set_digest(&questions),
        policy_digest: policy.digest(),
        evaluator_instruction_version: if route
            .as_ref()
            .is_some_and(|route| route.protocol.is_native())
        {
            "harn.evaluator.native.v2".into()
        } else {
            structured::EVALUATOR_INSTRUCTION_VERSION.into()
        },
        output_schema_version: structured::OUTPUT_SCHEMA_VERSION.into(),
        backend_kind: policy.backend.as_str().into(),
        protocol: route
            .as_ref()
            .map(|route| route.protocol.as_str().to_string())
            .unwrap_or_else(|| "unresolved".into()),
    };
    let identity_json = serde_json::to_string(&identity).expect("evaluation identity serializes");
    let evaluation_id = digest_parts([id.as_str(), identity_json.as_str()].into_iter());

    let mut receipt = EvaluationReceipt::not_dispatched(
        evaluation_id,
        id.clone(),
        identity,
        policy.provider.clone(),
        policy.model.clone(),
        &questions,
        "unavailable",
        0,
    );
    let reference = receipt.reference();

    // --- Local refusals. Every one of these makes zero provider requests. ---

    let refuse = |receipt: &mut EvaluationReceipt, outcome: Outcome| {
        receipt.outcome_kind = outcome.kind.into();
        receipt.elapsed_ms = started.elapsed().as_millis() as u64;
        outcome
    };

    let Some(route) = route else {
        let outcome = outcome::unavailable(&reference, "model_unconfigured");
        let outcome = refuse(&mut receipt, outcome);
        publish(&receipt);
        return Ok((outcome, Vec::new(), policy, receipt));
    };

    if let Err(refusal) = questions.admit(&route) {
        let outcome =
            outcome::question_invalid(&reference, &refusal.question, refusal.reason.as_str());
        let outcome = refuse(&mut receipt, outcome);
        publish(&receipt);
        return Ok((outcome, Vec::new(), policy, receipt));
    }

    // The estimate is the evaluator's own, and it is on the receipt whether or
    // not the provider later disagrees, so the estimator can be tuned from
    // real data instead of guessed at.
    let estimated = estimate_state_tokens(&state);
    receipt.estimated_state_tokens = Some(estimated);
    receipt.limit_tokens = Some(route.limits.state_window_tokens);
    if estimated > route.limits.state_window_tokens {
        let outcome =
            outcome::state_too_large(&reference, route.limits.state_window_tokens, estimated);
        let outcome = refuse(&mut receipt, outcome);
        publish(&receipt);
        return Ok((outcome, Vec::new(), policy, receipt));
    }

    // A price the route does not declare cannot be admitted, and an
    // unadmitted charge is not a free one.
    // Native input-only billing covers the entire request, including every
    // question. Encoded UTF-8 bytes bound token count without trusting a
    // tokenizer estimate for budget admission.
    let budget_input = if route.protocol.is_native() {
        let request = DecisionRequest {
            model: &policy.model,
            provider: &policy.provider,
            state: &state,
            questions: &questions,
            contract: &route,
            effort: &policy.effort,
            temperature: policy.temperature,
            evaluation_cost_limit: policy.evaluation_cost_limit,
            run_cost_limit: policy.run_cost_limit,
        };
        match native::request_body(&request) {
            Ok(body) => {
                let longest = body["questions"]
                    .as_object()
                    .into_iter()
                    .flat_map(|questions| questions.values())
                    .map(estimate_state_tokens)
                    .max()
                    .unwrap_or(0);
                let combined = estimated.saturating_add(longest);
                let total = estimate_state_tokens(&body);
                receipt.estimated_longest_question_tokens = Some(longest);
                receipt.estimated_request_tokens = Some(total);
                let exceeded = if combined > route.limits.state_window_tokens {
                    Some((route.limits.state_window_tokens, combined))
                } else {
                    route
                        .limits
                        .request_window_tokens
                        .filter(|limit| total > *limit)
                        .map(|limit| (limit, total))
                };
                if let Some((limit, estimated_tokens)) = exceeded {
                    receipt.limit_tokens = Some(limit);
                    let outcome = refuse(
                        &mut receipt,
                        outcome::state_too_large(&reference, limit, estimated_tokens),
                    );
                    publish(&receipt);
                    return Ok((outcome, Vec::new(), policy, receipt));
                }
                crate::canonical_json::to_vec(&body).len()
            }
            Err(_) => {
                let outcome = refuse(
                    &mut receipt,
                    outcome::unavailable(&reference, "unsupported_options"),
                );
                publish(&receipt);
                return Ok((outcome, Vec::new(), policy, receipt));
            }
        }
    } else {
        estimated
    };
    let Some(bound) = admitted_cost_bound(&route, budget_input) else {
        let outcome = outcome::unavailable(&reference, "unsupported_options");
        let outcome = refuse(&mut receipt, outcome);
        publish(&receipt);
        return Ok((outcome, Vec::new(), policy, receipt));
    };
    if let Some(limit) = policy.evaluation_cost_limit.filter(|limit| bound > *limit) {
        let outcome = outcome::budget_cut(&reference, "evaluation_cost", bound, limit);
        let outcome = refuse(&mut receipt, outcome);
        publish(&receipt);
        return Ok((outcome, Vec::new(), policy, receipt));
    }
    let spent = crate::llm::cost::peek_total_cost();
    if let Some(limit) = policy.run_cost_limit.filter(|limit| spent + bound > *limit) {
        let outcome = outcome::budget_cut(&reference, "run_cost", bound, (limit - spent).max(0.0));
        let outcome = refuse(&mut receipt, outcome);
        publish(&receipt);
        return Ok((outcome, Vec::new(), policy, receipt));
    }

    // An accepted stop is observed before dispatch, so a cancelled run does
    // not pay for an answer no branch will read.
    let (cancelled, deadline) = ctx.interrupt_sources();
    if cancelled
        .as_ref()
        .is_some_and(|flag| flag.load(Ordering::Relaxed))
    {
        let outcome = outcome::cancelled(&reference, "interrupt");
        let outcome = refuse(&mut receipt, outcome);
        publish(&receipt);
        return Ok((outcome, Vec::new(), policy, receipt));
    }
    if deadline.is_some_and(|deadline| deadline <= std::time::Instant::now()) {
        let outcome = outcome::budget_cut(&reference, "deadline", 0.0, 0.0);
        let outcome = refuse(&mut receipt, outcome);
        publish(&receipt);
        return Ok((outcome, Vec::new(), policy, receipt));
    }

    // --- One dispatch. ---

    let reservation = if policy.backend == BackendKind::NativeDecision {
        match super::admission::reserve_decision(
            bound,
            policy.evaluation_cost_limit,
            policy.run_cost_limit,
        ) {
            Ok(hold) => Some(hold),
            Err(_) => {
                let outcome = refuse(&mut receipt, native_admission_failure(&reference, bound));
                publish(&receipt);
                return Ok((outcome, Vec::new(), policy, receipt));
            }
        }
    } else {
        None
    };

    let evaluation = Evaluation {
        state,
        questions,
        policy,
    };
    let (mut outcome, answers) = dispatch(&evaluation, &route, &mut receipt, &reference).await;
    if let Some(hold) = reservation {
        let settled = if receipt.physical_attempts == 0 {
            Some(0.0)
        } else {
            receipt.cost_usd
        };
        if receipt
            .native_transport
            .as_ref()
            .is_some_and(|transport| transport.provider_attempts_reported.is_some_and(|n| n > 1))
        {
            hold.retain_contract_violation();
        } else if hold.settle(settled).is_err() {
            outcome = native_admission_failure(&reference, bound);
        }
    }
    if evaluation.policy.backend == BackendKind::NativeDecision && receipt.physical_attempts > 0 {
        // Unknown usage keeps the admitted amount, never a free failed call.
        let charged = receipt.cost_usd.unwrap_or(bound);
        receipt.cost_usd = Some(charged);
        if crate::llm::cost::accumulate_llm_usage(
            &evaluation.policy.model,
            receipt.input_tokens.unwrap_or(0).min(i64::MAX as u64) as i64,
            receipt.output_tokens.unwrap_or(0).min(i64::MAX as u64) as i64,
            charged,
        )
        .is_err()
        {
            outcome = native_admission_failure(&reference, charged);
        }
    }
    receipt.outcome_kind = outcome.kind.into();
    receipt.elapsed_ms = started.elapsed().as_millis() as u64;
    receipt.record_answers(&answers);
    publish(&receipt);
    Ok((outcome, answers, evaluation.policy, receipt))
}

fn native_admission_failure(reference: &str, requested: f64) -> Outcome {
    match super::admission::remaining_allowance() {
        Some(remaining) => outcome::budget_cut(reference, "run_cost", requested, remaining),
        None => outcome::unavailable(reference, "authority_denied"),
    }
}

async fn dispatch(
    evaluation: &Evaluation,
    route: &DecisionContract,
    receipt: &mut EvaluationReceipt,
    reference: &str,
) -> (Outcome, Vec<Answer>) {
    if evaluation.policy.backend == BackendKind::NativeDecision
        && evaluation.policy.native_options_supplied
    {
        return (
            outcome::unavailable(reference, "unsupported_options"),
            Vec::new(),
        );
    }
    if (evaluation.policy.backend == BackendKind::NativeDecision) != route.protocol.is_native() {
        return (
            outcome::unavailable(reference, "unsupported_options"),
            Vec::new(),
        );
    }
    let backend: Arc<dyn DecisionBackend> = match installed::get() {
        Some(installed) => installed,
        None => match evaluation.policy.backend {
            BackendKind::StructuredLlm => Arc::new(structured::StructuredLlmBackend),
            BackendKind::NativeDecision => Arc::new(native::NativeDecisionBackend),
        },
    };
    let request = DecisionRequest {
        model: &evaluation.policy.model,
        provider: &evaluation.policy.provider,
        state: &evaluation.state,
        questions: &evaluation.questions,
        contract: route,
        effort: &evaluation.policy.effort,
        temperature: evaluation.policy.temperature,
        evaluation_cost_limit: evaluation.policy.evaluation_cost_limit,
        run_cost_limit: evaluation.policy.run_cost_limit,
    };
    if evaluation.policy.backend == BackendKind::NativeDecision {
        receipt.native_transport = Some(receipt::NativeTransportReceipt {
            data_controls: native::privacy_plan(&request).receipt,
            provider_attempts_reported: None,
            final_provider_reported: None,
        });
    }
    let response = backend.evaluate(request).await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            receipt.physical_attempts = physical_attempts_for(&error);
            receipt.accounting_status = if receipt.physical_attempts == 0 {
                receipt.native_transport = None;
                AccountingStatus::NotDispatched
            } else {
                AccountingStatus::UsageUnknown
            };
            return (transport_outcome(error, receipt, reference), Vec::new());
        }
    };
    receipt.physical_attempts = response.physical_attempts;
    receipt.native_transport = response.native_transport.clone();
    receipt.served_model = response.served_model.clone();
    receipt.input_tokens = response.input_tokens;
    receipt.output_tokens = response.output_tokens;
    receipt.source = EvaluationSource::Live;
    if response.physical_attempts != 1 {
        return (
            outcome::refused(
                reference,
                "schema_invalid",
                "decision backend violated the single-request contract",
            ),
            Vec::new(),
        );
    }
    if let Some(usage) = response.usage {
        apply_structured_usage(receipt, usage);
    } else {
        match (response.input_tokens, response.output_tokens) {
            // Unknown usage keeps its reservation and says so. It is never
            // recorded as a free attempt.
            (Some(input), Some(output)) => {
                receipt.accounting_status = AccountingStatus::Settled;
                receipt.cost_usd = settled_cost(route, input, output);
            }
            _ => receipt.accounting_status = AccountingStatus::UsageUnknown,
        }
    }

    if receipt.native_transport.as_ref().is_some_and(|transport| {
        transport
            .provider_attempts_reported
            .is_some_and(|attempts| attempts > 1)
    }) {
        receipt.accounting_status = AccountingStatus::UsageUnknown;
        receipt.cost_usd = None;
        return (
            outcome::refused(
                reference,
                "provider_refusal",
                "gateway reported multiple downstream attempts",
            ),
            Vec::new(),
        );
    }

    // A partial answer set is a schema failure, not a smaller `answered`.
    if response.answers.len() != evaluation.questions.questions.len() {
        return (
            outcome::refused(
                reference,
                RefusalReason::SchemaInvalid.as_str(),
                &format!(
                    "backend answered {} of {} declared questions",
                    response.answers.len(),
                    evaluation.questions.questions.len()
                ),
            ),
            Vec::new(),
        );
    }

    let mut answers = Vec::with_capacity(evaluation.questions.questions.len());
    for question in &evaluation.questions.questions {
        let Some(raw) = response.answers.get(&question.id) else {
            return (
                outcome::refused(
                    reference,
                    RefusalReason::SchemaInvalid.as_str(),
                    &format!("backend omitted question `{}`", question.id),
                ),
                Vec::new(),
            );
        };
        match Answer::project(question, raw, response.provenance) {
            Ok(answer) => answers.push(answer),
            Err(rejection) => {
                return (
                    outcome::refused(
                        reference,
                        RefusalReason::SchemaInvalid.as_str(),
                        &format!(
                            "question `{}`: {}",
                            rejection.question_id, rejection.diagnostic
                        ),
                    ),
                    Vec::new(),
                )
            }
        }
    }

    let threshold = evaluation.policy.threshold;
    if answers.iter().all(|answer| answer.meets(threshold)) {
        (outcome::answered(reference, &answers), answers)
    } else {
        (
            outcome::low_confidence(reference, &answers, threshold),
            answers,
        )
    }
}

/// A transport error that never left the process made no request. Everything
/// else did, and a request that failed is still a request the provider may
/// have billed.
fn physical_attempts_for(error: &DecisionTransportError) -> u32 {
    match error {
        DecisionTransportError::Accounted { usage, .. } => usage
            .provider_call_count
            .and_then(|count| u32::try_from(count).ok())
            .unwrap_or(1),
        DecisionTransportError::UnsupportedOptions { .. }
        | DecisionTransportError::LocalAdmissionDenied { .. }
        | DecisionTransportError::AuthorityDenied => 0,
        _ => 1,
    }
}

fn transport_outcome(
    error: DecisionTransportError,
    receipt: &mut EvaluationReceipt,
    reference: &str,
) -> Outcome {
    match error {
        DecisionTransportError::Accounted {
            error,
            usage,
            served_model,
        } => {
            receipt.served_model = served_model;
            apply_structured_usage(receipt, usage);
            transport_outcome(*error, receipt, reference)
        }
        DecisionTransportError::AuthorityDenied => {
            outcome::unavailable(reference, "authority_denied")
        }
        DecisionTransportError::LocalAdmissionDenied { diagnostic } => {
            receipt.provider_reason = Some(diagnostic);
            outcome::unavailable(reference, "authority_denied")
        }
        DecisionTransportError::Refused { reason, diagnostic } => {
            outcome::refused(reference, reason.as_str(), &diagnostic)
        }
        DecisionTransportError::StateTooLarge {
            provider_reason,
            limit_tokens,
        } => {
            // The provider disagreed with the local estimate. Both numbers stay
            // on the receipt so the estimator can be corrected rather than
            // padded by guesswork.
            receipt.provider_reason = Some(provider_reason);
            let limit = limit_tokens.or(receipt.limit_tokens).unwrap_or_default();
            let estimated = receipt.estimated_state_tokens.unwrap_or_default();
            outcome::state_too_large(reference, limit, estimated)
        }
        DecisionTransportError::RateLimited { retry_after_ms } => {
            outcome::rate_limited(reference, retry_after_ms)
        }
        DecisionTransportError::Overloaded => outcome::overloaded(reference),
        DecisionTransportError::UnsupportedOptions { diagnostic } => {
            receipt.provider_reason = Some(diagnostic);
            outcome::unavailable(reference, "unsupported_options")
        }
        DecisionTransportError::TransportFailed { diagnostic } => {
            let authority = diagnostic.contains("api key")
                || diagnostic.contains("API key")
                || diagnostic.contains("unauthorized")
                || diagnostic.contains("forbidden");
            receipt.provider_reason = Some(diagnostic);
            outcome::unavailable(
                reference,
                if authority {
                    "authority_denied"
                } else {
                    "transport_failed"
                },
            )
        }
    }
}

fn apply_structured_usage(
    receipt: &mut EvaluationReceipt,
    usage: Box<crate::llm::usage::LlmUsage>,
) {
    receipt.cost_usd = usage.cost_usd;
    receipt.accounting_status = if usage.cost_usd.is_some() && usage.usage_unknown_calls == 0 {
        AccountingStatus::Settled
    } else {
        AccountingStatus::UsageUnknown
    };
    if usage.usage_unknown_calls == 0 {
        receipt.input_tokens = u64::try_from(usage.input_tokens).ok();
        receipt.output_tokens = u64::try_from(usage.output_tokens).ok();
    }
    receipt.usage = Some(usage);
}

/// The upper bound this evaluation may cost under the route's declared price.
/// `None` when the price is unknown, which refuses dispatch.
fn admitted_cost_bound(route: &DecisionContract, estimated_state_tokens: usize) -> Option<f64> {
    // Native protocols here have no output-token cap. Their monetary bound
    // is valid only for an explicitly declared input-only billing contract.
    if route.protocol.is_native() && route.output_price_per_mtok != Some(0.0) {
        return None;
    }
    let input = route.input_price_per_mtok?;
    let output = route.output_price_per_mtok.unwrap_or(0.0);
    if !input.is_finite() || !output.is_finite() {
        return None;
    }
    let input_tokens = estimated_state_tokens as f64 + 2048.0;
    let output_tokens = 2048.0;
    Some((input_tokens * input + output_tokens * output) / 1_000_000.0)
}

fn settled_cost(route: &DecisionContract, input: u64, output: u64) -> Option<f64> {
    let input_price = route.input_price_per_mtok?;
    let output_price = route.output_price_per_mtok.unwrap_or(0.0);
    Some((input as f64 * input_price + output as f64 * output_price) / 1_000_000.0)
}

/// The evaluator's estimate of a state, and the only one.
///
/// `harness.llm.estimate_state_tokens` calls this, and so does the ceiling a
/// few hundred lines up. A second implementation anywhere would let a caller
/// size its input against a ruler the refusal never uses.
pub(crate) fn estimate_state_tokens(state: &serde_json::Value) -> usize {
    let encoded = crate::canonical_json::to_vec(state);
    crate::llm::estimate_text_tokens(&String::from_utf8_lossy(&encoded)).max(0) as usize
}

fn state_type_name(state: &serde_json::Value) -> &'static str {
    match state {
        serde_json::Value::Object(_) => "record",
        serde_json::Value::Array(_) => "list",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Null => "nil",
    }
}

/// Put the receipt on the run journal. A caller only ever holds the reference,
/// and the journal is the only place the contents exist.
fn publish(receipt: &EvaluationReceipt) {
    let Ok(serde_json::Value::Object(payload)) = serde_json::to_value(receipt) else {
        return;
    };
    #[cfg(test)]
    last_receipt::record(receipt.clone());
    crate::events::log_info_meta(
        "llm.evaluation",
        &format!(
            "evaluation {} settled as {}",
            receipt.site_id, receipt.outcome_kind
        ),
        payload.into_iter().collect(),
    );
}

/// The most recent receipt this process produced. A test reads it to assert on
/// what was recorded, because the journal sink is not available to one.
#[cfg(test)]
mod last_receipt {
    use super::receipt::EvaluationReceipt;
    use std::sync::{Mutex, OnceLock};

    static LAST: OnceLock<Mutex<Option<EvaluationReceipt>>> = OnceLock::new();

    fn slot() -> &'static Mutex<Option<EvaluationReceipt>> {
        LAST.get_or_init(|| Mutex::new(None))
    }

    pub(super) fn record(receipt: EvaluationReceipt) {
        *slot().lock().expect("evaluation receipt slot") = Some(receipt);
    }

    #[cfg(test)]
    pub(crate) fn peek() -> Option<EvaluationReceipt> {
        slot().lock().expect("evaluation receipt slot").clone()
    }
}

#[cfg(test)]
pub(crate) use last_receipt::peek as last_receipt;

/// The single-boolean projection. One boolean question named by the site.
pub(crate) fn predicate_arguments(args: &[VmValue]) -> Result<Vec<VmValue>, VmError> {
    let [id, question, input, policy] = args else {
        return Err(VmError::Runtime(
            "evaluate_predicate expects (id, question, input, policy)".into(),
        ));
    };
    let questions = VmValue::dict(vec![(
        id.as_str_cow().into_owned().as_str(),
        VmValue::dict(vec![
            ("kind", VmValue::String("boolean".into())),
            ("instructions", question.clone()),
        ]),
    )]);
    Ok(vec![id.clone(), input.clone(), questions, policy.clone()])
}
