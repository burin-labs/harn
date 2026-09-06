use std::collections::BTreeSet;

use harn_parser::{parse_source, Node, TypeExpr};

use super::support::ProtocolArtifactSource;

pub(super) const EXTERNAL_ACTION_VOCABULARY_SOURCE: &str =
    "crates/harn-stdlib/src/stdlib/external_action/vocabulary.harn";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExternalActionVocabulary {
    pub(super) records: Vec<super::records::Record>,
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
    pub(super) progress_activity_statuses: Vec<String>,
    pub(super) terminal_activity_statuses: Vec<String>,
    pub(super) policy_layers: Vec<String>,
    pub(super) policy_evaluation_outcomes: Vec<String>,
    pub(super) decision_outcomes: Vec<String>,
    pub(super) deciders: Vec<String>,
    pub(super) reconciliation_statuses: Vec<String>,
}

impl ExternalActionVocabulary {
    /// Public enum and constant names shared by the three activity host bindings.
    pub(super) fn projections(&self) -> [(&str, &str, &[String]); 16] {
        [
            ("outcomes", "Outcome", &self.outcomes),
            ("receipt_statuses", "ReceiptStatus", &self.receipt_statuses),
            ("next_actions", "NextAction", &self.next_actions),
            ("environments", "Environment", &self.environments),
            (
                "authorization_methods",
                "AuthorizationMethod",
                &self.authorization_methods,
            ),
            (
                "authentication_assurances",
                "AuthenticationAssurance",
                &self.authentication_assurances,
            ),
            (
                "disclosure_sources",
                "DisclosureSource",
                &self.disclosure_sources,
            ),
            ("error_kinds", "ErrorKind", &self.error_kinds),
            (
                "protected_field_classes",
                "ProtectedFieldClass",
                &self.protected_field_classes,
            ),
            (
                "passenger_genders",
                "PassengerGender",
                &self.passenger_genders,
            ),
            (
                "activity_statuses",
                "ActivityStatus",
                &self.activity_statuses,
            ),
            ("policy_layers", "PolicyLayer", &self.policy_layers),
            (
                "policy_evaluation_outcomes",
                "PolicyEvaluationOutcome",
                &self.policy_evaluation_outcomes,
            ),
            (
                "decision_outcomes",
                "DecisionOutcome",
                &self.decision_outcomes,
            ),
            ("deciders", "Decider", &self.deciders),
            (
                "reconciliation_statuses",
                "ReconciliationStatus",
                &self.reconciliation_statuses,
            ),
        ]
    }

    pub(super) fn load(source: &ProtocolArtifactSource) -> Result<Self, String> {
        let text = source.read_text(EXTERNAL_ACTION_VOCABULARY_SOURCE)?;
        let mut vocabulary = Self::parse(&text)?;
        vocabulary.records = super::harn_records::load(
            source,
            &[
                "external_action/contracts",
                "external_action/disclosure",
                "external_action/activity",
            ],
            &[
                "ExternalActionActor",
                "ExternalActionMoney",
                "ExternalActionDisclosureReceipt",
                "ExternalActionError",
                "ExternalActionRetryLink",
                "ExternalActionReceipt",
                "ExternalActionPolicyEvaluation",
                "ExternalActionDecision",
                "ExternalActionAuthorizationRecord",
                "ExternalActionRequester",
                "ExternalActionDispatchRecord",
                "ExternalActionReconciliationRecord",
                "ExternalActionActivityRecord",
            ],
        )?;
        Ok(vocabulary)
    }

    fn parse(source: &str) -> Result<Self, String> {
        let program = parse_source(source).map_err(|error| {
            format!("failed to parse {EXTERNAL_ACTION_VOCABULARY_SOURCE}: {error}")
        })?;
        let values = |name| literal_union(&program, name, EXTERNAL_ACTION_VOCABULARY_SOURCE);
        let vocabulary = Self {
            records: Vec::new(),
            outcomes: values("ExternalActionOutcome")?,
            receipt_statuses: values("ExternalActionReceiptStatus")?,
            next_actions: values("ExternalActionNextAction")?,
            environments: values("ExternalActionEnvironment")?,
            authorization_methods: values("ExternalActionAuthorizationMethod")?,
            authentication_assurances: values("ExternalActionAuthenticationAssurance")?,
            disclosure_sources: values("ExternalActionDisclosureSource")?,
            error_kinds: values("ExternalActionErrorKind")?,
            protected_field_classes: values("ExternalActionProtectedFieldClass")?,
            passenger_genders: values("ExternalActionPassengerGender")?,
            activity_statuses: values("ExternalActionActivityStatus")?,
            progress_activity_statuses: values("ExternalActionProgressActivityStatus")?,
            terminal_activity_statuses: values("ExternalActionTerminalActivityStatus")?,
            policy_layers: values("ExternalActionPolicyLayer")?,
            policy_evaluation_outcomes: values("ExternalActionPolicyEvaluationOutcome")?,
            decision_outcomes: values("ExternalActionDecisionOutcome")?,
            deciders: values("ExternalActionDecider")?,
            reconciliation_statuses: values("ExternalActionReconciliationStatus")?,
        };
        let all = vocabulary.activity_statuses.iter().collect::<BTreeSet<_>>();
        let progress = vocabulary
            .progress_activity_statuses
            .iter()
            .collect::<BTreeSet<_>>();
        let terminal = vocabulary
            .terminal_activity_statuses
            .iter()
            .collect::<BTreeSet<_>>();
        if let Some(status) = progress.intersection(&terminal).next() {
            return Err(format!(
                "activity status `{status}` is both progress and terminal"
            ));
        }
        let partition = progress.union(&terminal).copied().collect::<BTreeSet<_>>();
        if partition != all {
            return Err("progress and terminal activity statuses must exactly partition all activity statuses".into());
        }
        Ok(vocabulary)
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
pub type ExternalActionActivityStatus = "proposed" | "denied"
pub type ExternalActionProgressActivityStatus = "proposed"
pub type ExternalActionTerminalActivityStatus = "denied"
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
        assert_eq!(vocabulary.progress_activity_statuses, ["proposed"]);
        assert_eq!(vocabulary.terminal_activity_statuses, ["denied"]);
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
pub type ExternalActionProgressActivityStatus = "proposed"
pub type ExternalActionTerminalActivityStatus = "denied"
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

    #[test]
    fn rejects_terminal_status_outside_activity_vocabulary() {
        let error = ExternalActionVocabulary::parse(
            r#"
pub type ExternalActionOutcome = "confirmed"
pub type ExternalActionReceiptStatus = "confirmed"
pub type ExternalActionNextAction = "none"
pub type ExternalActionEnvironment = "test"
pub type ExternalActionAuthorizationMethod = "manual"
pub type ExternalActionAuthenticationAssurance = "session"
pub type ExternalActionDisclosureSource = "user_profile"
pub type ExternalActionErrorKind = "invalid_intent"
pub type ExternalActionProtectedFieldClass = "legal_identity"
pub type ExternalActionPassengerGender = "m"
pub type ExternalActionActivityStatus = "proposed"
pub type ExternalActionProgressActivityStatus = "proposed"
pub type ExternalActionTerminalActivityStatus = "confirmed"
pub type ExternalActionPolicyLayer = "user_policy"
pub type ExternalActionPolicyEvaluationOutcome = "allowed"
pub type ExternalActionDecisionOutcome = "approved"
pub type ExternalActionDecider = "person"
pub type ExternalActionReconciliationStatus = "not_needed"
"#,
        )
        .unwrap_err();
        assert!(error.contains("exactly partition all activity statuses"));
    }
}
