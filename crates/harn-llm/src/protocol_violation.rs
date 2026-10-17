use serde::{Deserialize, Serialize};

/// Stable classification for a text-tool protocol violation.
///
/// The Harn stdlib owns detection and recovery policy. Rust mirrors the
/// structural result at the host boundary so callers cannot accidentally
/// collapse protocol identity into presentation text.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolViolationKind {
    AmbiguousRecovery,
    BareJson,
    EmptyDone,
    FenceDialect,
    ProviderDialect,
    RecoveredDialect,
    StrayText,
    UnclosedResponse,
    UnknownTag,
    UnparsedToolSyntax,
    WrongToolFormat,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ProtocolViolation {
    pub kind: ProtocolViolationKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped_reason: Option<String>,
}

impl ProtocolViolation {
    pub fn is_unparsed_tool_syntax(&self) -> bool {
        self.kind == ProtocolViolationKind::UnparsedToolSyntax
    }
}

#[cfg(test)]
mod tests {
    use super::{ProtocolViolation, ProtocolViolationKind};

    #[test]
    fn known_kind_round_trips_structurally() {
        let violation = ProtocolViolation {
            kind: ProtocolViolationKind::UnparsedToolSyntax,
            message: "unrecognized tool syntax".to_string(),
            excerpt: Some("<tool_call>".to_string()),
            dropped_reason: Some("unrecognized".to_string()),
        };

        let encoded = serde_json::to_value(&violation).expect("serialize violation");
        assert_eq!(encoded["kind"], "unparsed_tool_syntax");
        assert_eq!(
            serde_json::from_value::<ProtocolViolation>(encoded).expect("deserialize violation"),
            violation
        );
    }

    #[test]
    fn unknown_kind_is_rejected() {
        let encoded = serde_json::json!({
            "kind": "future_violation",
            "message": "unknown"
        });
        assert!(serde_json::from_value::<ProtocolViolation>(encoded).is_err());
    }
}
