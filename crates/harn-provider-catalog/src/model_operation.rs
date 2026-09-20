//! Closed operation vocabulary owned by the provider catalog.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelOperation {
    TextGeneration,
    Embedding,
    Decision,
}

impl ModelOperation {
    pub const ALL: [Self; 3] = [Self::TextGeneration, Self::Embedding, Self::Decision];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TextGeneration => "text_generation",
            Self::Embedding => "embedding",
            Self::Decision => "decision",
        }
    }

    pub const fn output_modality(self) -> &'static str {
        match self {
            Self::TextGeneration => "text",
            Self::Embedding => "embedding",
            Self::Decision => "decision",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_operation_roundtrips_without_inheriting_text() {
        let model: crate::ModelDef = serde_json::from_value(serde_json::json!({
            "name": "Decision fixture", "provider": "fixture", "context_window": 8192,
            "operations": ["decision"]
        }))
        .unwrap();
        assert_eq!(model.normalized_operations(), [ModelOperation::Decision]);
        assert_eq!(
            serde_json::to_value(&model).unwrap()["operations"],
            serde_json::json!(["decision"])
        );
        assert!(!model.supports_operation(ModelOperation::TextGeneration));
    }

    #[test]
    fn unknown_operation_is_not_a_legacy_text_row() {
        let result = serde_json::from_value::<crate::ModelDef>(serde_json::json!({
            "name": "Unknown fixture", "provider": "fixture", "context_window": 8192,
            "operations": ["unknown_operation"]
        }));
        assert!(result.is_err());
    }
}
