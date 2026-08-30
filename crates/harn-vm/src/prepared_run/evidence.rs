use std::collections::BTreeMap;
use std::path::{Component, Path};

use serde::Serialize;

use crate::orchestration::PolicyEvaluation;

use super::*;

pub(super) fn policy_evidence(
    fingerprint: &str,
    evaluation: &PolicyEvaluation,
) -> PolicyDecisionEvidence {
    PolicyDecisionEvidence {
        requirement_fingerprint: fingerprint.to_string(),
        action: evaluation.action.clone(),
        reason: evaluation.reason.clone(),
        matched_rule_id: evaluation
            .matched_rule
            .as_ref()
            .and_then(|rule| rule.id.clone()),
        risk_labels: evaluation.risk_labels.clone(),
        policy_decision: evaluation.receipt.clone(),
    }
}

pub(super) fn approval_batch(
    plan_fingerprint: &str,
    candidates: &[(ReceiptedAuthority, PolicyEvaluation)],
) -> Option<ApprovalBatch> {
    if candidates.is_empty() {
        return None;
    }
    let mut grouped: BTreeMap<String, ApprovalGroup> = BTreeMap::new();
    for (authority, evaluation) in candidates {
        let semantic_group = semantic_group(&authority.requirement).to_string();
        let group = grouped
            .entry(semantic_group.clone())
            .or_insert_with(|| ApprovalGroup {
                semantic_group,
                requirement_fingerprints: Vec::new(),
                summaries: Vec::new(),
                risk_labels: Vec::new(),
            });
        group
            .requirement_fingerprints
            .push(authority.fingerprint.clone());
        group
            .summaries
            .push(requirement_summary(&authority.requirement));
        group.risk_labels.extend(evaluation.risk_labels.clone());
    }
    let mut groups = grouped.into_values().collect::<Vec<_>>();
    for group in &mut groups {
        group.requirement_fingerprints.sort();
        group.summaries.sort();
        group.risk_labels.sort();
        group.risk_labels.dedup();
    }
    let batch_fingerprint = fingerprint(
        "harn authority approval batch v1",
        &(plan_fingerprint, &groups),
    )
    .expect("approval groups serialize");
    Some(ApprovalBatch {
        batch_fingerprint,
        plan_fingerprint: plan_fingerprint.to_string(),
        groups,
    })
}

fn semantic_group(requirement: &AuthorityRequirement) -> &'static str {
    match requirement {
        AuthorityRequirement::FilesystemRead { .. }
        | AuthorityRequirement::FilesystemWrite { .. } => "filesystem",
        AuthorityRequirement::ProcessReadRoot { .. }
        | AuthorityRequirement::ProcessWriteRoot { .. }
        | AuthorityRequirement::ProcessSandbox { .. }
        | AuthorityRequirement::ProcessSocket(_)
        | AuthorityRequirement::ToolchainProbe(_) => "process",
        AuthorityRequirement::Network(_) => "network",
        AuthorityRequirement::Secret(_)
        | AuthorityRequirement::IdentityBroker(_)
        | AuthorityRequirement::Environment { .. } => "credentials_and_environment",
        AuthorityRequirement::Mcp(_) => "mcp",
        AuthorityRequirement::Tool { .. }
        | AuthorityRequirement::HostCapability { .. }
        | AuthorityRequirement::SideEffectCeiling { .. } => "host_capabilities",
        AuthorityRequirement::Budget { .. } | AuthorityRequirement::RecursionLimit { .. } => {
            "budgets"
        }
        AuthorityRequirement::Provenance { .. } | AuthorityRequirement::Startup { .. } => {
            "runtime_and_startup"
        }
    }
}

fn requirement_summary(requirement: &AuthorityRequirement) -> String {
    match requirement {
        AuthorityRequirement::Secret(secret) => format!(
            "secret reference {} -> {:?}:{}",
            secret.reference, secret.consumer.kind, secret.consumer.id
        ),
        AuthorityRequirement::IdentityBroker(identity) => format!(
            "identity reference {} via {} -> {}:{} for audience {} tenant {:?}",
            identity.reference,
            identity.broker_id,
            identity.binding.provider,
            identity.binding.consumer.id,
            identity.binding.audience,
            identity.binding.tenant,
        ),
        other => serde_json::to_string(other).expect("authority requirement serializes"),
    }
}

pub(super) fn receipted_requirements(
    requirements: &[AuthorityRequirement],
) -> Vec<ReceiptedAuthority> {
    let mut authority = requirements
        .iter()
        .cloned()
        .map(|requirement| ReceiptedAuthority {
            fingerprint: requirement_fingerprint(&requirement),
            requirement,
        })
        .collect::<Vec<_>>();
    authority.sort_by(|left, right| left.fingerprint.cmp(&right.fingerprint));
    authority
}

pub fn requirement_fingerprint(requirement: &AuthorityRequirement) -> String {
    fingerprint("harn authority requirement v1", requirement)
        .expect("authority requirements are serializable")
}

pub(super) fn fingerprint(domain: &str, value: &impl Serialize) -> Result<String, String> {
    let canonical = crate::canonical_json::of(value).map_err(|error| error.to_string())?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain.as_bytes());
    hasher.update(b"\n");
    hasher.update(canonical.as_bytes());
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

pub(super) fn requirement_attenuates(
    granted: &AuthorityRequirement,
    requested: &AuthorityRequirement,
) -> bool {
    match (granted, requested) {
        (
            AuthorityRequirement::FilesystemRead { root: granted },
            AuthorityRequirement::FilesystemRead { root: requested },
        )
        | (
            AuthorityRequirement::FilesystemWrite { root: granted },
            AuthorityRequirement::FilesystemWrite { root: requested },
        )
        | (
            AuthorityRequirement::FilesystemWrite { root: granted },
            AuthorityRequirement::FilesystemRead { root: requested },
        )
        | (
            AuthorityRequirement::ProcessReadRoot { root: granted },
            AuthorityRequirement::ProcessReadRoot { root: requested },
        )
        | (
            AuthorityRequirement::ProcessWriteRoot { root: granted },
            AuthorityRequirement::ProcessWriteRoot { root: requested },
        )
        | (
            AuthorityRequirement::ProcessWriteRoot { root: granted },
            AuthorityRequirement::ProcessReadRoot { root: requested },
        ) => path_is_within(requested, granted),
        (
            AuthorityRequirement::ToolchainProbe(probe),
            AuthorityRequirement::ProcessReadRoot { root: requested },
        ) => path_is_within(requested, &probe.read_root_ceiling),
        _ => granted == requested,
    }
}

fn path_is_within(requested: &str, granted: &str) -> bool {
    let Some(requested) = lexical_components(Path::new(requested)) else {
        return false;
    };
    let Some(granted) = lexical_components(Path::new(granted)) else {
        return false;
    };
    requested.len() >= granted.len() && requested[..granted.len()] == granted
}

fn lexical_components(path: &Path) -> Option<Vec<String>> {
    let mut normalized = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                normalized.push(prefix.as_os_str().to_string_lossy().to_string());
            }
            Component::RootDir => normalized.push("/".to_string()),
            Component::CurDir => {}
            Component::ParentDir => match normalized.last().map(String::as_str) {
                Some("/") | None => return None,
                Some(_) => {
                    normalized.pop();
                }
            },
            Component::Normal(value) => {
                normalized.push(value.to_string_lossy().to_string());
            }
        }
    }
    Some(normalized)
}

pub(super) fn diagnostic(
    code: impl Into<String>,
    message: impl Into<String>,
    requirement_fingerprint: Option<String>,
    actionable: impl Into<String>,
) -> AuthorityDiagnostic {
    AuthorityDiagnostic {
        code: code.into(),
        message: message.into(),
        requirement_fingerprint,
        actionable: actionable.into(),
    }
}

pub(super) fn persist_terminally(
    sink: &dyn AuthorityReceiptSink,
    diagnostics: &mut Vec<AuthorityDiagnostic>,
    receipt: &RunAuthorityReceipt,
) {
    if let Err(error) = sink.persist(receipt) {
        diagnostics.push(diagnostic(
            "outcome_receipt_persistence",
            error,
            None,
            "Repair receipt persistence before retrying the run.",
        ));
    }
}
