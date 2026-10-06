//! The `[providers.capabilities]` contract.
//!
//! Split out of `manifest.rs` because it owns a self-contained decision -- how
//! a connector declares which protocol features it speaks -- and reads in
//! either of two authoring shapes.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ConnectorCapabilities {
    pub webhook: bool,
    pub oauth: bool,
    pub rate_limit: bool,
    pub pagination: bool,
    pub graphql: bool,
    pub streaming: bool,
}

impl ConnectorCapabilities {
    pub const FEATURES: [&'static str; 6] = [
        "webhook",
        "oauth",
        "rate_limit",
        "pagination",
        "graphql",
        "streaming",
    ];

    fn enable(&mut self, feature: &str) -> Result<(), String> {
        match normalize_connector_capability(feature).as_str() {
            "webhook" => self.webhook = true,
            "oauth" => self.oauth = true,
            "rate_limit" => self.rate_limit = true,
            "pagination" => self.pagination = true,
            "graphql" => self.graphql = true,
            "streaming" => self.streaming = true,
            other => {
                return Err(format!(
                    "unknown connector capability '{feature}' (normalized as '{other}')"
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectorCapabilitiesTable {
    #[serde(default)]
    webhook: bool,
    #[serde(default)]
    oauth: bool,
    #[serde(default, alias = "rate-limit")]
    rate_limit: bool,
    #[serde(default)]
    pagination: bool,
    #[serde(default)]
    graphql: bool,
    #[serde(default)]
    streaming: bool,
}

impl From<ConnectorCapabilitiesTable> for ConnectorCapabilities {
    fn from(value: ConnectorCapabilitiesTable) -> Self {
        Self {
            webhook: value.webhook,
            oauth: value.oauth,
            rate_limit: value.rate_limit,
            pagination: value.pagination,
            graphql: value.graphql,
            streaming: value.streaming,
        }
    }
}

/// `capabilities` accepts either a list of feature names or a table of
/// booleans, dispatched on the value's own shape.
///
/// A `#[serde(untagged)]` enum would also accept both, but untagged buffers the
/// input and reports "data did not match any variant" whenever every branch
/// fails — discarding the precise error the failing branch produced. Since both
/// branches fail closed on an unrecognized feature, that generic message is the
/// one an author with a typo would see. Dispatching on the shape keeps the
/// branch's own error.
impl<'de> Deserialize<'de> for ConnectorCapabilities {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct CapabilitiesVisitor;

        impl<'de> serde::de::Visitor<'de> for CapabilitiesVisitor {
            type Value = ConnectorCapabilities;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a list of connector capability names or a table of booleans")
            }

            fn visit_seq<A>(self, sequence: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let features = Vec::<String>::deserialize(
                    serde::de::value::SeqAccessDeserializer::new(sequence),
                )?;
                let mut capabilities = ConnectorCapabilities::default();
                for feature in features {
                    capabilities
                        .enable(&feature)
                        .map_err(serde::de::Error::custom)?;
                }
                Ok(capabilities)
            }

            fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                ConnectorCapabilitiesTable::deserialize(
                    serde::de::value::MapAccessDeserializer::new(map),
                )
                .map(ConnectorCapabilities::from)
            }
        }

        deserializer.deserialize_any(CapabilitiesVisitor)
    }
}

pub fn normalize_connector_capability(feature: &str) -> String {
    feature.trim().to_lowercase().replace('-', "_")
}
