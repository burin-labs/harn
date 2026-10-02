//! The explicit handler outcome shared by agent and public tool adapters.

use crate::value::{ErrorCategory, VmError, VmValue};

/// The versioned result discriminator consumed by the runtime and linter.
pub const AGENT_TOOL_HANDLER_RESULT_SCHEMA: &str = "harn.agent_tool_handler_result.v2";

/// Project resolved return types that declare the complete explicit envelope.
/// Imported aliases and local structural types share this discriminator.
pub fn tool_handler_output_schema(schema: serde_json::Value) -> serde_json::Value {
    let canonical = (|| {
        if schema.get("type")?.as_str()? != "object" {
            return None;
        }
        let properties = schema.get("properties")?.as_object()?;
        let discriminator = properties.get("schema")?;
        if discriminator.get("type")?.as_str()? != "string"
            || discriminator.get("const")?.as_str()? != AGENT_TOOL_HANDLER_RESULT_SCHEMA
            || properties.get("text")?.get("type")?.as_str()? != "string"
        {
            return None;
        }
        let outcome = properties.get("outcome")?;
        let alternatives = outcome.get("enum")?.as_array()?;
        if outcome.get("type")?.as_str()? != "string"
            || alternatives.len() != 3
            || !["ok", "error", "rejected"].iter().all(|name| {
                alternatives
                    .iter()
                    .any(|value| value.as_str() == Some(name))
            })
        {
            return None;
        }
        let required = schema.get("required")?.as_array()?;
        if !["schema", "outcome", "text", "data"]
            .iter()
            .all(|name| required.iter().any(|value| value.as_str() == Some(name)))
        {
            return None;
        }
        properties.get("data").cloned()
    })();
    canonical.unwrap_or(schema)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HandlerOutcome {
    Ok,
    Error,
    Rejected,
}

impl HandlerOutcome {
    pub(crate) fn failure_category(self) -> Option<&'static str> {
        match self {
            Self::Ok => None,
            Self::Error => Some("tool_error"),
            Self::Rejected => Some("tool_rejected"),
        }
    }
}

pub(crate) struct HandlerResult<'a> {
    pub text: &'a str,
    pub data: &'a VmValue,
    pub outcome: HandlerOutcome,
}

pub(crate) fn invalid_handler_result() -> VmError {
    VmError::CategorizedError {
        message: concat!(
            "tool handler must return a typed outcome: use ",
            "agent_tool_handler_result(text, data, outcome), or a nominal struct ",
            "with exactly one boolean ok or success field"
        )
        .into(),
        category: ErrorCategory::SchemaValidation,
    }
}

/// Recognize only the explicit versioned contract, never payload conventions.
pub(crate) fn parse_handler_result(value: &VmValue) -> Result<Option<HandlerResult<'_>>, VmError> {
    let field = |name: &str| match value {
        VmValue::Dict(fields) => fields.get(name),
        _ => value.struct_field(name),
    };
    if !matches!(field("schema"), Some(VmValue::String(schema))
        if schema.as_str() == AGENT_TOOL_HANDLER_RESULT_SCHEMA)
    {
        return Ok(None);
    }
    let Some(VmValue::String(text)) = field("text") else {
        return Err(invalid_handler_result());
    };
    let Some(data) = field("data") else {
        return Err(invalid_handler_result());
    };
    let outcome = match field("outcome") {
        Some(VmValue::String(outcome)) => match outcome.as_ref() {
            "ok" => HandlerOutcome::Ok,
            "error" => HandlerOutcome::Error,
            "rejected" => HandlerOutcome::Rejected,
            _ => return Err(invalid_handler_result()),
        },
        _ => return Err(invalid_handler_result()),
    };
    Ok(Some(HandlerResult {
        text,
        data,
        outcome,
    }))
}

pub(crate) fn agent_tool_handler_result_text(value: &serde_json::Value) -> Option<&str> {
    let object = value.as_object()?;
    if object.get("schema")?.as_str()? != AGENT_TOOL_HANDLER_RESULT_SCHEMA {
        return None;
    }
    object.get("text")?.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn export_schema_projection_requires_the_complete_versioned_contract() {
        let schema = json!({
            "type": "object", "properties": {
                "schema": {"type": "string", "const": AGENT_TOOL_HANDLER_RESULT_SCHEMA},
                "text": {"type": "string"},
                "outcome": {"type": "string", "enum": ["ok", "error", "rejected"]},
                "data": {"type": "integer"}
            }, "required": ["schema", "outcome", "text", "data"]
        });
        assert_eq!(
            tool_handler_output_schema(schema.clone()),
            json!({"type": "integer"})
        );
        let mut wrong_version = schema.clone();
        wrong_version["properties"]["schema"]["const"] = json!("ordinary.domain.v2");
        let mut open_outcome = schema.clone();
        open_outcome["properties"]["outcome"]
            .as_object_mut()
            .unwrap()
            .remove("enum");
        let mut optional_data = schema.clone();
        optional_data["required"] = json!(["schema", "outcome", "text"]);
        let mut wrong_text = schema;
        wrong_text["properties"]["text"] = json!({"type": "object"});
        for ordinary in [wrong_version, open_outcome, optional_data, wrong_text] {
            assert_eq!(tool_handler_output_schema(ordinary.clone()), ordinary);
        }
    }
}
