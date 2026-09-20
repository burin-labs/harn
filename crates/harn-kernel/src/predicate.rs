//! Portable projection of checked predicate sites. The frontend owns site
//! discovery and type admission; this layer adds stable content identities.

use harn_parser::{canonical_predicate_type, PredicateSite};
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
    pub question_sha256: String,
    pub input_type_sha256: String,
    pub outcome_schema: String,
    pub effects: Vec<String>,
    pub line: usize,
    pub column: usize,
}

impl PredicateManifest {
    /// Called only after source analysis completes. A failed parse is not an
    /// empty manifest: the containing check report must keep it absent.
    pub fn from_checked_sites(source: &str, sites: &[PredicateSite]) -> Self {
        Self {
            schema: "harn.predicate_sites.v1".into(),
            sites: sites
                .iter()
                .map(|site| PredicateManifestSite {
                    source: source.into(),
                    id: site.id.clone(),
                    question_sha256: crate::pure::sha256_hex(site.question.as_bytes()),
                    input_type_sha256: crate::pure::sha256_hex(
                        canonical_predicate_type(&site.input_type).as_bytes(),
                    ),
                    outcome_schema: "harn.predicate.outcome.v1".into(),
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
    use harn_parser::{ShapeField, TypeExpr};

    fn site(fields: Vec<ShapeField>) -> PredicateSite {
        PredicateSite {
            model_route: None,
            id: "finding.v1".into(),
            question: "Supported?".into(),
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
        assert_eq!(
            first.sites[0].question_sha256,
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
