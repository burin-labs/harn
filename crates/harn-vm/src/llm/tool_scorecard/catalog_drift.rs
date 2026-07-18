use super::{ToolScorecardCatalogClaim, ToolScorecardCatalogMismatch, ToolScorecardCatalogUpdate};

pub(super) fn evaluate(
    claim: &Option<ToolScorecardCatalogClaim>,
    recommended_tool_mode: &str,
) -> (
    Vec<ToolScorecardCatalogMismatch>,
    Vec<ToolScorecardCatalogUpdate>,
) {
    let Some(claim) = claim else {
        return (
            vec![ToolScorecardCatalogMismatch {
                code: "route_missing_from_catalog",
                field: "provider_catalog.models",
                observed: recommended_tool_mode.to_string(),
                catalog: None,
            }],
            Vec::new(),
        );
    };

    let mut mismatches = Vec::new();
    let mut updates = Vec::new();
    let preferred = claim.preferred_tool_format.as_deref();
    let recommended_preferred_format = recommended_preferred_format(recommended_tool_mode);

    if let (Some(preferred), Some(recommended_preferred_format)) =
        (preferred, recommended_preferred_format)
    {
        if !tool_format_matches_mode(preferred, recommended_tool_mode) {
            mismatches.push(ToolScorecardCatalogMismatch {
                code: "preferred_tool_format_disagrees",
                field: "tool_support.preferred_format",
                observed: recommended_tool_mode.to_string(),
                catalog: Some(preferred.to_string()),
            });
            updates.push(set_catalog_update(
                "tool_support.preferred_format",
                recommended_preferred_format.to_string(),
                "scorecard_recommended_tool_mode",
            ));
        }
    }

    match recommended_tool_mode {
        "native" if !claim.native_tools => {
            mismatches.push(ToolScorecardCatalogMismatch {
                code: "observed_native_not_cataloged",
                field: "tool_support.native",
                observed: "true".to_string(),
                catalog: Some("false".to_string()),
            });
            updates.push(set_catalog_update(
                "tool_support.native",
                "true".to_string(),
                "scorecard_observed_native_tool_calls",
            ));
        }
        "text" if !claim.text_tools => {
            mismatches.push(ToolScorecardCatalogMismatch {
                code: "observed_text_not_cataloged",
                field: "tool_support.text",
                observed: "true".to_string(),
                catalog: Some("false".to_string()),
            });
            updates.push(set_catalog_update(
                "tool_support.text",
                "true".to_string(),
                "scorecard_observed_text_tool_calls",
            ));
        }
        _ => {}
    }

    (mismatches, updates)
}

fn tool_format_matches_mode(format: &str, recommended_tool_mode: &str) -> bool {
    let Some(format_channel) = crate::llm_config::tool_format_channel(format) else {
        return false;
    };
    matches!(
        (format_channel, recommended_tool_mode),
        (crate::llm_config::ToolFormatChannel::Native, "native")
            | (crate::llm_config::ToolFormatChannel::Text, "text")
    )
}

fn recommended_preferred_format(recommended_tool_mode: &str) -> Option<&'static str> {
    match recommended_tool_mode {
        "native" => Some("native"),
        // A text-channel observation does not distinguish heredoc from fenced
        // JSON, so retain Harn's safer global text-channel default.
        "text" => Some("json"),
        _ => None,
    }
}

fn set_catalog_update(
    field: &'static str,
    value: String,
    reason: &'static str,
) -> ToolScorecardCatalogUpdate {
    ToolScorecardCatalogUpdate {
        field,
        operation: "set",
        value: Some(value),
        reason,
    }
}
