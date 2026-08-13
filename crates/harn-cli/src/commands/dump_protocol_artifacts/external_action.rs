use std::collections::BTreeSet;

use harn_parser::{parse_source, Node, TypeExpr};

use super::support::ProtocolArtifactSource;

pub(super) const EXTERNAL_ACTION_VOCABULARY_SOURCE: &str =
    "crates/harn-stdlib/src/stdlib/external_action/vocabulary.harn";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExternalActionVocabulary {
    pub(super) outcomes: Vec<String>,
    pub(super) receipt_statuses: Vec<String>,
    pub(super) next_actions: Vec<String>,
    pub(super) environments: Vec<String>,
    pub(super) authorization_methods: Vec<String>,
    pub(super) authentication_assurances: Vec<String>,
    pub(super) disclosure_sources: Vec<String>,
    pub(super) error_kinds: Vec<String>,
    pub(super) protected_field_classes: Vec<String>,
    pub(super) passenger_genders: Vec<String>,
    pub(super) activity_statuses: Vec<String>,
    pub(super) policy_layers: Vec<String>,
    pub(super) policy_evaluation_outcomes: Vec<String>,
    pub(super) decision_outcomes: Vec<String>,
    pub(super) deciders: Vec<String>,
    pub(super) reconciliation_statuses: Vec<String>,
}

impl ExternalActionVocabulary {
    pub(super) fn load(source: &ProtocolArtifactSource) -> Result<Self, String> {
        let text = source.read_text(EXTERNAL_ACTION_VOCABULARY_SOURCE)?;
        Self::parse(&text)
    }

    fn parse(source: &str) -> Result<Self, String> {
        let program = parse_source(source).map_err(|error| {
            format!("failed to parse {EXTERNAL_ACTION_VOCABULARY_SOURCE}: {error}")
        })?;
        Ok(Self {
            outcomes: literal_union(
                &program,
                "ExternalActionOutcome",
                EXTERNAL_ACTION_VOCABULARY_SOURCE,
            )?,
            receipt_statuses: literal_union(
                &program,
                "ExternalActionReceiptStatus",
                EXTERNAL_ACTION_VOCABULARY_SOURCE,
            )?,
            next_actions: literal_union(
                &program,
                "ExternalActionNextAction",
                EXTERNAL_ACTION_VOCABULARY_SOURCE,
            )?,
            environments: literal_union(
                &program,
                "ExternalActionEnvironment",
                EXTERNAL_ACTION_VOCABULARY_SOURCE,
            )?,
            authorization_methods: literal_union(
                &program,
                "ExternalActionAuthorizationMethod",
                EXTERNAL_ACTION_VOCABULARY_SOURCE,
            )?,
            authentication_assurances: literal_union(
                &program,
                "ExternalActionAuthenticationAssurance",
                EXTERNAL_ACTION_VOCABULARY_SOURCE,
            )?,
            disclosure_sources: literal_union(
                &program,
                "ExternalActionDisclosureSource",
                EXTERNAL_ACTION_VOCABULARY_SOURCE,
            )?,
            error_kinds: literal_union(
                &program,
                "ExternalActionErrorKind",
                EXTERNAL_ACTION_VOCABULARY_SOURCE,
            )?,
            protected_field_classes: literal_union(
                &program,
                "ExternalActionProtectedFieldClass",
                EXTERNAL_ACTION_VOCABULARY_SOURCE,
            )?,
            passenger_genders: literal_union(
                &program,
                "ExternalActionPassengerGender",
                EXTERNAL_ACTION_VOCABULARY_SOURCE,
            )?,
            activity_statuses: literal_union(
                &program,
                "ExternalActionActivityStatus",
                EXTERNAL_ACTION_VOCABULARY_SOURCE,
            )?,
            policy_layers: literal_union(
                &program,
                "ExternalActionPolicyLayer",
                EXTERNAL_ACTION_VOCABULARY_SOURCE,
            )?,
            policy_evaluation_outcomes: literal_union(
                &program,
                "ExternalActionPolicyEvaluationOutcome",
                EXTERNAL_ACTION_VOCABULARY_SOURCE,
            )?,
            decision_outcomes: literal_union(
                &program,
                "ExternalActionDecisionOutcome",
                EXTERNAL_ACTION_VOCABULARY_SOURCE,
            )?,
            deciders: literal_union(
                &program,
                "ExternalActionDecider",
                EXTERNAL_ACTION_VOCABULARY_SOURCE,
            )?,
            reconciliation_statuses: literal_union(
                &program,
                "ExternalActionReconciliationStatus",
                EXTERNAL_ACTION_VOCABULARY_SOURCE,
            )?,
        })
    }

    #[cfg(test)]
    pub(super) fn load_for_tests() -> Self {
        let source =
            ProtocolArtifactSource::from_anchor(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
                .expect("harn-cli is compiled from the Harn workspace");
        Self::load(&source).expect("external-action vocabulary is valid")
    }
}

pub(super) fn literal_union(
    program: &[harn_parser::SNode],
    type_name: &str,
    source_path: &str,
) -> Result<Vec<String>, String> {
    let type_expr = program.iter().find_map(|node| match &node.node {
        Node::TypeDecl {
            name, type_expr, ..
        } if name == type_name => Some(type_expr),
        _ => None,
    });
    let Some(type_expr) = type_expr else {
        return Err(format!("{source_path} must declare `pub type {type_name}`"));
    };

    let values: Vec<String> = match type_expr {
        TypeExpr::LitString(value) => vec![value.clone()],
        TypeExpr::Union(members) => members
            .iter()
            .map(|member| match member {
                TypeExpr::LitString(value) => Ok(value.clone()),
                other => Err(format!(
                    "{type_name} must contain only string literals, found {other:?}"
                )),
            })
            .collect::<Result<_, _>>()?,
        other => {
            return Err(format!(
                "{type_name} must be a closed string-literal union, found {other:?}"
            ));
        }
    };
    if values.is_empty() {
        return Err(format!("{type_name} must contain at least one value"));
    }
    let mut seen = BTreeSet::new();
    for value in &values {
        if value.is_empty()
            || !value
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
        {
            return Err(format!(
                "{type_name} value `{value}` must be non-empty lower snake case"
            ));
        }
        if !seen.insert(value) {
            return Err(format!("{type_name} repeats `{value}`"));
        }
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_closed_literal_unions_in_source_order() {
        let vocabulary = ExternalActionVocabulary::parse(
            r#"
pub type ExternalActionOutcome = "first" | "second_value"
pub type ExternalActionReceiptStatus = "ready"
pub type ExternalActionNextAction = "none" | "continue"
pub type ExternalActionEnvironment = "test"
pub type ExternalActionAuthorizationMethod = "manual"
pub type ExternalActionAuthenticationAssurance = "session"
pub type ExternalActionDisclosureSource = "user_profile"
pub type ExternalActionErrorKind = "invalid_intent"
pub type ExternalActionProtectedFieldClass = "legal_identity"
pub type ExternalActionPassengerGender = "m" | "f"
pub type ExternalActionActivityStatus = "proposed"
pub type ExternalActionPolicyLayer = "user_policy"
pub type ExternalActionPolicyEvaluationOutcome = "allowed"
pub type ExternalActionDecisionOutcome = "approved"
pub type ExternalActionDecider = "person"
pub type ExternalActionReconciliationStatus = "not_needed"
"#,
        )
        .unwrap();
        assert_eq!(vocabulary.outcomes, ["first", "second_value"]);
        assert_eq!(vocabulary.receipt_statuses, ["ready"]);
        assert_eq!(vocabulary.next_actions, ["none", "continue"]);
        assert_eq!(vocabulary.environments, ["test"]);
        assert_eq!(vocabulary.authorization_methods, ["manual"]);
        assert_eq!(vocabulary.protected_field_classes, ["legal_identity"]);
        assert_eq!(vocabulary.passenger_genders, ["m", "f"]);
    }

    #[test]
    fn rejects_open_vocabulary() {
        let open = ExternalActionVocabulary::parse(
            r#"
pub type ExternalActionOutcome = string
pub type ExternalActionReceiptStatus = "ready"
pub type ExternalActionNextAction = "none"
pub type ExternalActionEnvironment = "test"
pub type ExternalActionAuthorizationMethod = "manual"
pub type ExternalActionAuthenticationAssurance = "session"
pub type ExternalActionDisclosureSource = "user_profile"
pub type ExternalActionErrorKind = "invalid_intent"
pub type ExternalActionProtectedFieldClass = "legal_identity"
pub type ExternalActionPassengerGender = "m"
pub type ExternalActionActivityStatus = "proposed"
pub type ExternalActionPolicyLayer = "user_policy"
pub type ExternalActionPolicyEvaluationOutcome = "allowed"
pub type ExternalActionDecisionOutcome = "approved"
pub type ExternalActionDecider = "person"
pub type ExternalActionReconciliationStatus = "not_needed"
"#,
        )
        .unwrap_err();
        assert!(open.contains("closed string-literal union"));
    }
}
