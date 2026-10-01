//! Portable projection of checked predicate sites. The frontend owns site
//! discovery and type admission; this layer adds stable content identities.

use harn_parser::{canonical_predicate_type, PredicateQuestionSpec, PredicateSite};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PredicateManifest {
    pub schema: String,
    pub sites: Vec<PredicateManifestSite>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PredicateManifestSite {
    pub source: String,
    pub id: String,
    /// Runtime-bound sites are declarations only; their actual questions and
    /// route are bound by the execution receipt, not this source census.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub runtime_admission: bool,
    /// One digest over the whole question set. This is the cache identity
    /// component a site's questions contribute; a reordered, renamed, or
    /// relabelled question set is a different evaluation.
    pub question_set_sha256: Option<String>,
    pub questions: Vec<PredicateManifestQuestion>,
    pub input_type_sha256: String,
    pub outcome_schema: String,
    pub effects: Vec<String>,
    pub line: usize,
    pub column: usize,
}

/// A question census entry. Instructions are hashed rather than carried: a
/// manifest travels further than the source it describes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PredicateManifestQuestion {
    pub id: String,
    pub kind: String,
    pub instructions_sha256: String,
    /// Criteria labels for a choice, levels for a score, zero for a boolean.
    pub option_count: usize,
}

/// A length-delimited, domain-separated encoding. Concatenating the parts
/// directly would let a renamed label and a lengthened id collide.
fn question_set_digest(questions: &[PredicateQuestionSpec]) -> String {
    let mut encoded = Vec::new();
    for question in questions {
        for part in [
            question.id.as_str(),
            question.kind.as_str(),
            question.instructions.as_str(),
        ] {
            encoded.extend_from_slice(&(part.len() as u64).to_le_bytes());
            encoded.extend_from_slice(part.as_bytes());
        }
        encoded.extend_from_slice(&(question.labels.len() as u64).to_le_bytes());
        for label in &question.labels {
            encoded.extend_from_slice(&(label.len() as u64).to_le_bytes());
            encoded.extend_from_slice(label.as_bytes());
        }
    }
    crate::pure::sha256_hex(&encoded)
}

impl PredicateManifest {
    /// Called only after source analysis completes. A failed parse is not an
    /// empty manifest: the containing check report must keep it absent.
    pub fn from_checked_sites(source: &str, sites: &[PredicateSite]) -> Self {
        Self {
            schema: "harn.predicate_sites.v2".into(),
            sites: sites
                .iter()
                .map(|site| PredicateManifestSite {
                    source: source.into(),
                    id: site.id.clone(),
                    runtime_admission: site.kind
                        == harn_parser::PredicateSiteKind::RuntimeEvaluation,
                    question_set_sha256: if site.kind
                        == harn_parser::PredicateSiteKind::RuntimeEvaluation
                    {
                        None
                    } else {
                        Some(question_set_digest(&site.questions))
                    },
                    questions: site
                        .questions
                        .iter()
                        .map(|question| PredicateManifestQuestion {
                            id: question.id.clone(),
                            kind: question.kind.as_str().into(),
                            instructions_sha256: crate::pure::sha256_hex(
                                question.instructions.as_bytes(),
                            ),
                            option_count: question.labels.len(),
                        })
                        .collect(),
                    input_type_sha256: crate::pure::sha256_hex(
                        canonical_predicate_type(&site.input_type).as_bytes(),
                    ),
                    outcome_schema: site.kind.outcome_schema().into(),
                    effects: vec!["llm.write".into()],
                    line: site.line,
                    column: site.column,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_parser::{PredicateQuestionKind, PredicateSiteKind, ShapeField, TypeExpr};

    fn site(fields: Vec<ShapeField>) -> PredicateSite {
        PredicateSite {
            model_route: None,
            id: "finding.v1".into(),
            kind: PredicateSiteKind::Predicate,
            questions: vec![PredicateQuestionSpec {
                id: "finding.v1".into(),
                kind: PredicateQuestionKind::Boolean,
                instructions: "Supported?".into(),
                labels: Vec::new(),
            }],
            input_type: TypeExpr::Shape(fields),
            line: 7,
            column: 3,
            start: 40,
            end: 90,
        }
    }

    #[test]
    fn manifest_hashes_semantics_and_retains_site_location() {
        let a = ShapeField::synthetic("a", TypeExpr::Named("string".into()), false);
        let b = ShapeField::synthetic("b", TypeExpr::Named("int".into()), false);
        let first =
            PredicateManifest::from_checked_sites("main.harn", &[site(vec![a.clone(), b.clone()])]);
        let reordered = PredicateManifest::from_checked_sites("main.harn", &[site(vec![b, a])]);
        assert_eq!(first, reordered);
        assert_eq!(first.sites.len(), 1);
        assert_eq!(first.sites[0].line, 7);
        assert_eq!(first.sites[0].questions.len(), 1);
        assert_eq!(first.sites[0].questions[0].kind, "boolean");
        assert_eq!(first.sites[0].questions[0].option_count, 0);
        assert_eq!(
            first.sites[0].questions[0].instructions_sha256,
            crate::pure::sha256_hex(b"Supported?")
        );
        let changed = PredicateManifest::from_checked_sites(
            "main.harn",
            &[site(vec![ShapeField::synthetic(
                "a",
                TypeExpr::Named("bool".into()),
                false,
            )])],
        );
        assert_ne!(
            first.sites[0].input_type_sha256,
            changed.sites[0].input_type_sha256
        );
        let bytes = serde_json::to_vec(&first).unwrap();
        assert_eq!(
            serde_json::from_slice::<PredicateManifest>(&bytes).unwrap(),
            first
        );
    }

    #[test]
    fn runtime_manifest_does_not_claim_an_empty_question_set_was_bound() {
        let mut runtime = site(vec![ShapeField::synthetic(
            "text",
            TypeExpr::Named("string".into()),
            false,
        )]);
        runtime.kind = PredicateSiteKind::RuntimeEvaluation;
        runtime.questions.clear();
        let manifest = PredicateManifest::from_checked_sites("runtime.harn", &[runtime]);
        assert!(manifest.sites[0].runtime_admission);
        assert_eq!(manifest.sites[0].question_set_sha256, None);
        let json = serde_json::to_value(manifest).unwrap();
        assert!(json["sites"][0]["question_set_sha256"].is_null());
    }

    fn choice(id: &str, labels: &[&str]) -> PredicateQuestionSpec {
        PredicateQuestionSpec {
            id: id.into(),
            kind: PredicateQuestionKind::Choice,
            instructions: "Which one?".into(),
            labels: labels.iter().map(|label| (*label).to_string()).collect(),
        }
    }

    #[test]
    fn question_set_digest_separates_parts_that_plain_concatenation_would_collide() {
        // `ab` + `c` and `a` + `bc` are the same bytes concatenated. A
        // length-delimited encoding keeps them distinct, so a relabelled
        // question cannot reuse another question set's cache identity.
        assert_ne!(
            question_set_digest(&[choice("q", &["ab", "c"])]),
            question_set_digest(&[choice("q", &["a", "bc"])]),
        );
        // Label order is significant: a choice's probabilities are keyed by
        // label and a score's levels are ordered.
        assert_ne!(
            question_set_digest(&[choice("q", &["a", "b"])]),
            question_set_digest(&[choice("q", &["b", "a"])]),
        );
        // A known non-null read through the same path, so the inequalities
        // above cannot be two empty digests comparing equal to nothing.
        assert_eq!(
            question_set_digest(&[choice("q", &["a", "b"])]),
            question_set_digest(&[choice("q", &["a", "b"])]),
        );
        assert_ne!(
            question_set_digest(&[]),
            question_set_digest(&[choice("q", &["a"])])
        );
    }

    #[test]
    fn manifest_effect_projection_matches_the_registered_contract() {
        use harn_builtin_meta::{EffectAccess, EffectKind};
        let entry = harn_capability_contracts::manifest()
            .iter()
            .find(|entry| entry.name == harn_builtin_meta::predicate::EVALUATE.name)
            .expect("predicate contract is registered");
        assert!(
            !entry.contract.effects.is_empty(),
            "absence is not a read-only evaluation"
        );
        assert!(entry
            .contract
            .effects
            .iter()
            .all(|effect| effect.kind == EffectKind::Llm && effect.access == EffectAccess::Write));
        let manifest = PredicateManifest::from_checked_sites("main.harn", &[site(vec![])]);
        assert_eq!(manifest.sites[0].effects, ["llm.write"]);
    }
}
