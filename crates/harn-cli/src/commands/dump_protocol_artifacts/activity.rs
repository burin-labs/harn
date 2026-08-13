use harn_parser::parse_source;

use super::external_action::literal_union;
use super::support::ProtocolArtifactSource;

pub(super) const ACTIVITY_VOCABULARY_SOURCE: &str =
    "crates/harn-stdlib/src/stdlib/activity/vocabulary.harn";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ActivityVocabulary {
    pub(super) kinds: Vec<String>,
    pub(super) permission_outcomes: Vec<String>,
    pub(super) permission_deciders: Vec<String>,
    pub(super) permission_policy_layers: Vec<String>,
    pub(super) permission_policy_outcomes: Vec<String>,
    pub(super) permission_grant_scopes: Vec<String>,
    pub(super) permission_grant_expiries: Vec<String>,
}

impl ActivityVocabulary {
    pub(super) fn load(source: &ProtocolArtifactSource) -> Result<Self, String> {
        let text = source.read_text(ACTIVITY_VOCABULARY_SOURCE)?;
        Self::parse(&text)
    }

    fn parse(source: &str) -> Result<Self, String> {
        let program = parse_source(source)
            .map_err(|error| format!("failed to parse {ACTIVITY_VOCABULARY_SOURCE}: {error}"))?;
        let union = |name| literal_union(&program, name, ACTIVITY_VOCABULARY_SOURCE);
        Ok(Self {
            kinds: union("ActivityKind")?,
            permission_outcomes: union("ToolPermissionOutcome")?,
            permission_deciders: union("ToolPermissionDecider")?,
            permission_policy_layers: union("ToolPermissionPolicyLayer")?,
            permission_policy_outcomes: union("ToolPermissionPolicyOutcome")?,
            permission_grant_scopes: union("ToolPermissionGrantScope")?,
            permission_grant_expiries: union("ToolPermissionGrantExpiry")?,
        })
    }

    #[cfg(test)]
    pub(super) fn load_for_tests() -> Self {
        let source =
            ProtocolArtifactSource::from_anchor(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
                .expect("harn-cli is compiled from the Harn workspace");
        Self::load(&source).expect("activity vocabulary is valid")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_vm::orchestration::{
        ActivityKind, ToolPermissionDecider, ToolPermissionGrantExpiry, ToolPermissionGrantScope,
        ToolPermissionOutcome, ToolPermissionPolicyLayer, ToolPermissionPolicyOutcome,
    };

    fn wire_values<T: serde::Serialize>(values: &[T]) -> Vec<String> {
        values
            .iter()
            .map(|value| {
                serde_json::to_value(value)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn parses_closed_activity_vocabulary() {
        let vocabulary = ActivityVocabulary::parse(
            r#"
pub type ActivityKind = "external_action" | "tool_permission"
pub type ToolPermissionOutcome = "approved"
pub type ToolPermissionDecider = "person"
pub type ToolPermissionPolicyLayer = "user_policy"
pub type ToolPermissionPolicyOutcome = "allowed"
pub type ToolPermissionGrantScope = "once"
pub type ToolPermissionGrantExpiry = "after_dispatch"
"#,
        )
        .unwrap();
        assert_eq!(vocabulary.kinds, ["external_action", "tool_permission"]);
        assert_eq!(vocabulary.permission_outcomes, ["approved"]);
        assert_eq!(vocabulary.permission_deciders, ["person"]);
    }

    #[test]
    fn runtime_projection_matches_harn_owned_vocabulary() {
        let vocabulary = ActivityVocabulary::load_for_tests();
        assert_eq!(vocabulary.kinds, wire_values(&ActivityKind::ALL));
        assert_eq!(
            vocabulary.permission_outcomes,
            wire_values(&ToolPermissionOutcome::ALL)
        );
        assert_eq!(
            vocabulary.permission_deciders,
            wire_values(&ToolPermissionDecider::ALL)
        );
        assert_eq!(
            vocabulary.permission_policy_layers,
            wire_values(&ToolPermissionPolicyLayer::ALL)
        );
        assert_eq!(
            vocabulary.permission_policy_outcomes,
            wire_values(&ToolPermissionPolicyOutcome::ALL)
        );
        assert_eq!(
            vocabulary.permission_grant_scopes,
            wire_values(&ToolPermissionGrantScope::ALL)
        );
        assert_eq!(
            vocabulary.permission_grant_expiries,
            wire_values(&ToolPermissionGrantExpiry::ALL)
        );
    }
}
