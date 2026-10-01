//! Canonical request identity and pure request-to-receipt verification.
//!
//! This verifies a binding, not a provider signature, model quality, accounting,
//! or an invocation's uniqueness. Historical receipts with an unknown contract
//! stay readable but cannot be upgraded to verified evidence by this API.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::receipt::{
    EvaluationIdentity, EvaluationReceipt, EVALUATION_IDENTITY_CONTRACT, EVALUATION_RECEIPT_SCHEMA,
};
use super::{contract::DecisionContract, question::QuestionSet, Evaluation, EvaluationPolicy};
use crate::value::{VmError, VmValue};

/// The same closed request record accepted by the evaluator CLI.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationRequest {
    pub site_id: String,
    pub state: Value,
    pub questions: Value,
    pub policy: Value,
}

impl EvaluationRequest {
    pub(super) fn arguments(&self) -> [VmValue; 4] {
        [
            VmValue::string(&self.site_id),
            crate::schema::json_to_vm_value(&self.state),
            crate::schema::json_to_vm_value(&self.questions),
            crate::schema::json_to_vm_value(&self.policy),
        ]
    }
}

pub(super) fn request_identity(
    state: &Value,
    questions: &QuestionSet,
    policy: &EvaluationPolicy,
    route: Option<&DecisionContract>,
) -> EvaluationIdentity {
    let canonical_state = crate::canonical_json::to_vec(state);
    EvaluationIdentity {
        contract_version: EVALUATION_IDENTITY_CONTRACT.into(),
        structured_output_strategy: route
            .and_then(|route| route.structured_output_strategy)
            .map(|strategy| strategy.as_str().to_string()),
        input_digest: super::digest_parts(std::iter::once(
            std::str::from_utf8(&canonical_state).expect("canonical JSON is UTF-8"),
        )),
        canonical_input_type: super::state_type_name(state).into(),
        question_set_digest: super::question_set_digest(questions),
        policy_digest: policy.digest(),
        evaluator_instruction_version: if route.is_some_and(|route| route.protocol.is_native()) {
            "harn.evaluator.native.v2".into()
        } else {
            super::structured::EVALUATOR_INSTRUCTION_VERSION.into()
        },
        output_schema_version: super::structured::OUTPUT_SCHEMA_VERSION.into(),
        backend_kind: policy.backend.as_str().into(),
        protocol: route
            .map(|route| route.protocol.as_str().to_string())
            .unwrap_or_else(|| "unresolved".into()),
    }
}

/// Stable request correlation, deliberately independent of occurrence count.
pub(super) fn request_id(site_id: &str, identity: &EvaluationIdentity) -> String {
    let encoded = serde_json::to_string(identity).expect("evaluation identity serializes");
    super::digest_parts([site_id, encoded.as_str()].into_iter())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationRefusalCode {
    UnsupportedContract,
    InputMismatch,
    QuestionsMismatch,
    PolicyMismatch,
    SiteMismatch,
    RouteMismatch,
    IdentityMismatch,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationRefusal {
    pub code: VerificationRefusalCode,
    pub field: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptVerification {
    pub schema: String,
    pub verified: bool,
    pub refusals: Vec<VerificationRefusal>,
    pub request_identity: Option<EvaluationIdentity>,
    pub stable_request_id: Option<String>,
}

/// Verify using the installed evaluator contract, without executing a VM,
/// requesting credentials, reading a budget, or dispatching a provider call.
/// A changed catalog protocol/strategy is an unsupported contract, not a
/// reinterpretation of historical evidence. Cache/replay source is preserved.
pub fn verify_receipt(
    request: &EvaluationRequest,
    receipt: &EvaluationReceipt,
) -> Result<ReceiptVerification, VmError> {
    use VerificationRefusalCode as Code;
    let mut report = ReceiptVerification {
        schema: "harn.evaluation_verification.v1".into(),
        verified: false,
        refusals: Vec::new(),
        request_identity: None,
        stable_request_id: None,
    };
    let mut check = |matches: bool, code, field: &str| {
        if !matches {
            report.refusals.push(VerificationRefusal {
                code,
                field: field.into(),
            });
        }
    };
    check(
        receipt.schema == EVALUATION_RECEIPT_SCHEMA,
        Code::UnsupportedContract,
        "schema",
    );
    check(
        receipt.identity.contract_version == EVALUATION_IDENTITY_CONTRACT,
        Code::UnsupportedContract,
        "identity.contract_version",
    );
    if !report.refusals.is_empty() {
        return Ok(report);
    }
    let (id, evaluation) = Evaluation::from_arguments(&request.arguments())?;
    let route = super::resolve_route(&evaluation.policy.provider, &evaluation.policy.model);
    let expected = request_identity(
        &evaluation.state,
        &evaluation.questions,
        &evaluation.policy,
        route.as_ref(),
    );
    let mut check = |matches: bool, code, field: &str| {
        if !matches {
            report.refusals.push(VerificationRefusal {
                code,
                field: field.into(),
            });
        }
    };
    for (actual, expected, field) in [
        (
            &receipt.identity.evaluator_instruction_version,
            &expected.evaluator_instruction_version,
            "identity.evaluator_instruction_version",
        ),
        (
            &receipt.identity.output_schema_version,
            &expected.output_schema_version,
            "identity.output_schema_version",
        ),
        (
            &receipt.identity.protocol,
            &expected.protocol,
            "identity.protocol",
        ),
    ] {
        check(actual == expected, Code::UnsupportedContract, field);
    }
    check(
        receipt.identity.structured_output_strategy == expected.structured_output_strategy,
        Code::UnsupportedContract,
        "identity.structured_output_strategy",
    );
    check(receipt.site_id == id, Code::SiteMismatch, "site_id");
    check(
        receipt.requested_provider == evaluation.policy.provider,
        Code::RouteMismatch,
        "requested_provider",
    );
    check(
        receipt.requested_model == evaluation.policy.model,
        Code::RouteMismatch,
        "requested_model",
    );
    check(
        receipt.identity.input_digest == expected.input_digest,
        Code::InputMismatch,
        "identity.input_digest",
    );
    check(
        receipt.identity.canonical_input_type == expected.canonical_input_type,
        Code::InputMismatch,
        "identity.canonical_input_type",
    );
    check(
        receipt.identity.question_set_digest == expected.question_set_digest,
        Code::QuestionsMismatch,
        "identity.question_set_digest",
    );
    check(
        receipt.identity.policy_digest == expected.policy_digest,
        Code::PolicyMismatch,
        "identity.policy_digest",
    );
    check(
        receipt.identity.backend_kind == expected.backend_kind,
        Code::PolicyMismatch,
        "identity.backend_kind",
    );
    let stable_id = request_id(&id, &expected);
    check(
        receipt.evaluation_id == stable_id,
        Code::IdentityMismatch,
        "evaluation_id",
    );
    report.verified = report.refusals.is_empty();
    report.request_identity = Some(expected);
    report.stable_request_id = Some(stable_id);
    Ok(report)
}
