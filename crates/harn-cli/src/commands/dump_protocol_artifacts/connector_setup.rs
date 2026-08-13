use harn_parser::parse_source;

use super::external_action::literal_union;
use super::support::ProtocolArtifactSource;

pub(super) const CONNECTOR_SETUP_VOCABULARY_SOURCE: &str =
    "crates/harn-stdlib/src/stdlib/connectors/setup.harn";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ConnectorSetupVocabulary {
    pub(super) stages: Vec<String>,
    pub(super) statuses: Vec<String>,
    pub(super) interactions: Vec<String>,
    pub(super) configuration_fields: Vec<String>,
    pub(super) error_codes: Vec<String>,
}

impl ConnectorSetupVocabulary {
    pub(super) fn load(source: &ProtocolArtifactSource) -> Result<Self, String> {
        let text = source.read_text(CONNECTOR_SETUP_VOCABULARY_SOURCE)?;
        Self::parse(&text)
    }

    fn parse(source: &str) -> Result<Self, String> {
        let program = parse_source(source).map_err(|error| {
            format!("failed to parse {CONNECTOR_SETUP_VOCABULARY_SOURCE}: {error}")
        })?;
        Ok(Self {
            stages: literal_union(
                &program,
                "ConnectorSetupStage",
                CONNECTOR_SETUP_VOCABULARY_SOURCE,
            )?,
            statuses: literal_union(
                &program,
                "ConnectorSetupStatus",
                CONNECTOR_SETUP_VOCABULARY_SOURCE,
            )?,
            interactions: literal_union(
                &program,
                "ConnectorSetupInteraction",
                CONNECTOR_SETUP_VOCABULARY_SOURCE,
            )?,
            configuration_fields: literal_union(
                &program,
                "ConnectorSetupConfigurationField",
                CONNECTOR_SETUP_VOCABULARY_SOURCE,
            )?,
            error_codes: literal_union(
                &program,
                "ConnectorSetupErrorCode",
                CONNECTOR_SETUP_VOCABULARY_SOURCE,
            )?,
        })
    }

    #[cfg(test)]
    pub(super) fn load_for_tests() -> Self {
        let source =
            ProtocolArtifactSource::from_anchor(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
                .expect("harn-cli is compiled from the Harn workspace");
        Self::load(&source).expect("connector-setup vocabulary is valid")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_closed_setup_vocabulary() {
        let vocabulary = ConnectorSetupVocabulary::parse(
            r#"
pub type ConnectorSetupStage = "resolving" | "ready"
pub type ConnectorSetupStatus = "in_progress" | "succeeded"
pub type ConnectorSetupInteraction = "none" | "browser"
pub type ConnectorSetupConfigurationField = "oauth_client_id"
pub type ConnectorSetupErrorCode = "configuration_missing" | "unknown"
"#,
        )
        .unwrap();
        assert_eq!(vocabulary.stages, ["resolving", "ready"]);
        assert_eq!(vocabulary.statuses, ["in_progress", "succeeded"]);
        assert_eq!(vocabulary.interactions, ["none", "browser"]);
        assert_eq!(vocabulary.configuration_fields, ["oauth_client_id"]);
        assert_eq!(vocabulary.error_codes, ["configuration_missing", "unknown"]);
    }
}
