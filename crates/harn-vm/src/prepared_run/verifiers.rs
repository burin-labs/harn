use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::verifier_provenance::{
    PreparedVerifier, VerifierBaselineRegistry, VerifierLoaderObservation, VerifierLoaderSpec,
};

use super::evidence::diagnostic;
use super::{AuthorityDiagnostic, RunAuthorityReceipt};

pub(super) fn bind_lease_fingerprint(
    parent: String,
    baselines: &BTreeMap<String, String>,
) -> String {
    if baselines.is_empty() {
        parent
    } else {
        super::evidence::fingerprint("harn verifier authority lease v1", &(&parent, baselines))
            .expect("verifier baseline identities are serializable")
    }
}

pub(super) fn capture(
    registry: &VerifierBaselineRegistry,
    requested: &[VerifierLoaderSpec],
    admitted: &BTreeSet<VerifierLoaderSpec>,
    receipt: &mut RunAuthorityReceipt,
    diagnostics: &mut Vec<AuthorityDiagnostic>,
) -> BTreeMap<String, Arc<PreparedVerifier>> {
    let mut verifiers = BTreeMap::new();
    for request in requested {
        let captured = if admitted.contains(request) {
            registry.capture(request.clone())
        } else {
            Err(VerifierLoaderObservation::Refused {
                reason: "verifier has no exact host admission".into(),
            })
        };
        match captured {
            Ok(verifier) if !verifiers.contains_key(request.id()) => {
                receipt.verifier_baselines.insert(
                    request.id().to_string(),
                    verifier.baseline_sha256().to_string(),
                );
                receipt
                    .verifier_observations
                    .insert(request.id().to_string(), verifier.observe_workspace());
                verifiers.insert(request.id().to_string(), verifier);
            }
            Ok(_) => diagnostics.push(diagnostic(
                "verifier_duplicate_id",
                "verifier loader id is repeated",
                None,
                "Declare each host-admitted verifier exactly once.",
            )),
            Err(error) => {
                receipt
                    .verifier_observations
                    .insert(request.id().to_string(), error.clone());
                diagnostics.push(diagnostic(
                    "verifier_unmeasured",
                    format!("{error:?}"),
                    None,
                    "Admit a supported verifier loader before executor mutation.",
                ));
            }
        }
    }
    verifiers
}
