use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HypothesisAuthorityKind {
    PlanAdmission,
    NativeApproval,
    NativeObservation,
    LifecycleAudit,
}

impl HypothesisAuthorityKind {
    pub(super) fn parse(value: &str, builtin: &str) -> Result<Self, VmError> {
        match value {
            "plan_admission" => Ok(Self::PlanAdmission),
            "native_approval" => Ok(Self::NativeApproval),
            "native_observation" => Ok(Self::NativeObservation),
            "lifecycle_audit" => Ok(Self::LifecycleAudit),
            _ => Err(VmError::TypeError(format!(
                "{builtin}: authority_kind must be plan_admission, native_approval, native_observation, or lifecycle_audit"
            ))),
        }
    }

    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::PlanAdmission => "plan_admission",
            Self::NativeApproval => "native_approval",
            Self::NativeObservation => "native_observation",
            Self::LifecycleAudit => "lifecycle_audit",
        }
    }

    pub(super) fn accepts(self, event_kind: &str) -> bool {
        match self {
            Self::PlanAdmission => event_kind == "plan_registered",
            Self::NativeApproval => event_kind == "approval_recorded",
            Self::NativeObservation => event_kind == "observation_recorded",
            Self::LifecycleAudit => matches!(
                event_kind,
                "run_transition"
                    | "decision_recorded"
                    | "relationship_recorded"
                    | "execution_drift"
                    | "invalidated"
                    | "regression_observed"
            ),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct HypothesisEventAuthorityProof {
    pub(super) authority_kind: HypothesisAuthorityKind,
    pub(super) event_fingerprint: Arc<str>,
    pub(super) plan_fingerprint: Arc<str>,
    pub(super) hypothesis_id: Arc<str>,
    pub(super) run_id: Option<Arc<str>>,
    pub(super) execution_scope: Option<Arc<str>>,
}

/// Host-issued, non-serializable evidence that a registered native adapter
/// completed the operation for which Harn will mint a scoped append proof.
#[derive(Clone, Debug)]
struct HypothesisNativeAttestation(HypothesisEventAuthorityProof);

pub fn mint_hypothesis_native_attestation(
    authority_kind: &str,
    event_fingerprint: &str,
    plan_fingerprint: &str,
    hypothesis_id: &str,
    run_id: Option<&str>,
) -> Result<VmValue, VmError> {
    const OWNER: &str = "native hypothesis adapter";
    let event_value = VmValue::String(arcstr::ArcStr::from(event_fingerprint));
    let plan_value = VmValue::String(arcstr::ArcStr::from(plan_fingerprint));
    let proof = HypothesisEventAuthorityProof {
        authority_kind: HypothesisAuthorityKind::parse(authority_kind, OWNER)?,
        event_fingerprint: Arc::from(required_sha256_fingerprint(
            Some(&event_value),
            OWNER,
            "event_fingerprint",
        )?),
        plan_fingerprint: Arc::from(required_sha256_fingerprint(
            Some(&plan_value),
            OWNER,
            "plan_fingerprint",
        )?),
        hypothesis_id: Arc::from(hypothesis_id),
        run_id: run_id.map(Arc::from),
        execution_scope: crate::observability::execution_scope::current_execution_scope(),
    };
    if proof.hypothesis_id.trim().is_empty()
        || proof
            .run_id
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
    {
        return Err(VmError::TypeError(format!(
            "{OWNER}: hypothesis_id and any run_id must be non-empty"
        )));
    }
    Ok(VmValue::resource(VmResourceHandle::new(
        HYPOTHESIS_NATIVE_ATTESTATION_HANDLE,
        HypothesisNativeAttestation(proof),
    )))
}

pub(super) fn proof_from_native_attestation(
    value: Option<&VmValue>,
    builtin: &str,
) -> Result<HypothesisEventAuthorityProof, VmError> {
    let VmValue::Resource(handle) = value.unwrap_or(&VmValue::Nil) else {
        return Err(VmError::TypeError(format!(
            "{builtin}: registered native adapter attestation resource is required"
        )));
    };
    (handle.label() == HYPOTHESIS_NATIVE_ATTESTATION_HANDLE)
        .then(|| handle.downcast::<HypothesisNativeAttestation>())
        .flatten()
        .map(|attestation| attestation.0.clone())
        .ok_or_else(|| {
            VmError::TypeError(format!(
                "{builtin}: registered native adapter attestation resource is required"
            ))
        })
}

pub(super) fn insert_hypothesis_authority_headers(
    headers: &mut BTreeMap<String, String>,
    proof: &HypothesisEventAuthorityProof,
) {
    headers.insert(
        HYPOTHESIS_AUTHORITY_SCHEMA_HEADER.to_string(),
        HYPOTHESIS_AUTHORITY_SCHEMA.to_string(),
    );
    headers.insert(
        HYPOTHESIS_AUTHORITY_KIND_HEADER.to_string(),
        proof.authority_kind.as_str().to_string(),
    );
    headers.insert(
        HYPOTHESIS_EVENT_FINGERPRINT_HEADER.to_string(),
        proof.event_fingerprint.to_string(),
    );
    headers.insert(
        HYPOTHESIS_PLAN_FINGERPRINT_HEADER.to_string(),
        proof.plan_fingerprint.to_string(),
    );
    headers.insert(
        HYPOTHESIS_ID_HEADER.to_string(),
        proof.hypothesis_id.to_string(),
    );
    if let Some(run_id) = &proof.run_id {
        headers.insert(HYPOTHESIS_RUN_ID_HEADER.to_string(), run_id.to_string());
    }
}

pub(super) fn verify_hypothesis_append_outcome(
    event: &LogEvent,
    proof: &HypothesisEventAuthorityProof,
    kind: &str,
    payload: &serde_json::Value,
    expected_headers: &BTreeMap<String, String>,
    builtin: &str,
) -> Result<(), VmError> {
    let required = [
        (
            HYPOTHESIS_AUTHORITY_SCHEMA_HEADER,
            HYPOTHESIS_AUTHORITY_SCHEMA,
        ),
        (
            HYPOTHESIS_AUTHORITY_KIND_HEADER,
            proof.authority_kind.as_str(),
        ),
        (
            HYPOTHESIS_EVENT_FINGERPRINT_HEADER,
            proof.event_fingerprint.as_ref(),
        ),
        (
            HYPOTHESIS_PLAN_FINGERPRINT_HEADER,
            proof.plan_fingerprint.as_ref(),
        ),
        (HYPOTHESIS_ID_HEADER, proof.hypothesis_id.as_ref()),
    ];
    let headers_match = required.iter().all(|(header, value)| {
        event
            .headers
            .get(*header)
            .is_some_and(|actual| actual.as_str() == *value)
    }) && match &proof.run_id {
        Some(run_id) => event
            .headers
            .get(HYPOTHESIS_RUN_ID_HEADER)
            .is_some_and(|actual| actual.as_str() == run_id.as_ref()),
        None => !event.headers.contains_key(HYPOTHESIS_RUN_ID_HEADER),
    };
    let persisted_headers: BTreeMap<&str, &str> = event
        .headers
        .iter()
        .filter(|(header, _)| !header.starts_with("harn.provenance."))
        .map(|(header, value)| (header.as_str(), value.as_str()))
        .collect();
    let expected_headers: BTreeMap<&str, &str> = expected_headers
        .iter()
        .map(|(header, value)| (header.as_str(), value.as_str()))
        .collect();
    if event.kind != kind
        || event.payload != *payload
        || !headers_match
        || persisted_headers != expected_headers
    {
        return Err(VmError::Runtime(format!(
            "{builtin}: idempotency key is already bound to a different hypothesis event"
        )));
    }
    Ok(())
}

fn required_string(value: &VmValue, field: &str, builtin: &str) -> Result<String, VmError> {
    match hypothesis_event_field(value, field) {
        Some(VmValue::String(value)) if !value.trim().is_empty() => Ok(value.to_string()),
        _ => Err(VmError::TypeError(format!(
            "{builtin}: hypothesis event content requires a non-empty string '{field}'"
        ))),
    }
}

fn optional_string(value: &VmValue, field: &str, builtin: &str) -> Result<Option<String>, VmError> {
    match hypothesis_event_field(value, field) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(value)) if !value.trim().is_empty() => Ok(Some(value.to_string())),
        _ => Err(VmError::TypeError(format!(
            "{builtin}: hypothesis event content field '{field}' must be a non-empty string or nil"
        ))),
    }
}

pub(super) fn normalize_hypothesis_projection_headers(
    headers: &mut BTreeMap<String, String>,
    event: &VmValue,
    proof: &HypothesisEventAuthorityProof,
    log_kind: &str,
    builtin: &str,
) -> Result<(), VmError> {
    let content = hypothesis_event_field(event, "content").ok_or_else(|| {
        VmError::TypeError(format!("{builtin}: missing hypothesis event content"))
    })?;
    let schema = required_string(content, "schema", builtin)?;
    let event_id = required_string(content, "event_id", builtin)?;
    let hypothesis_id = required_string(content, "hypothesis_id", builtin)?;
    let plan_id = optional_string(content, "plan_id", builtin)?;
    let run_id = optional_string(content, "run_id", builtin)?;
    let payload = hypothesis_event_field(content, "payload").ok_or_else(|| {
        VmError::TypeError(format!("{builtin}: missing hypothesis event payload"))
    })?;
    let payload_kind = required_string(payload, "kind", builtin)?;
    if schema != "harn.hypothesis.event.v1"
        || hypothesis_id != proof.hypothesis_id.as_ref()
        || run_id.as_deref() != proof.run_id.as_deref()
        || !proof.authority_kind.accepts(&payload_kind)
        || log_kind != format!("hypothesis.{payload_kind}")
    {
        return Err(VmError::Runtime(format!(
            "{builtin}: event projection does not match the native authority lane"
        )));
    }
    headers.insert("schema".to_string(), schema);
    headers.insert("event_id".to_string(), event_id);
    headers.insert("hypothesis_id".to_string(), hypothesis_id);
    headers.insert(
        "fingerprint".to_string(),
        proof.event_fingerprint.to_string(),
    );
    match plan_id {
        Some(value) => headers.insert("plan_id".to_string(), value),
        None => headers.remove("plan_id"),
    };
    match run_id {
        Some(value) => headers.insert("run_id".to_string(), value),
        None => headers.remove("run_id"),
    };
    Ok(())
}
