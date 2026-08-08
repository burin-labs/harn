//! `harn mcp presets [--json]` — render the harn-owned MCP preset catalog.
//!
//! The catalog itself lives in `harn_vm::mcp_presets` (the single source of
//! truth, alongside the rest of harn's MCP machinery). This module only
//! formats it: a human-readable table by default, or the stable JSON
//! envelope under `--json` for thin clients (IDE host TUI/GUI) to render
//! the same one-click server list.

use harn_vm::mcp_presets::{self, McpPreset, PresetCatalog};

use crate::cli::McpPresetsArgs;
use crate::json_envelope::{self, JsonEnvelope};

/// Schema version of the `harn mcp presets --json` envelope. Mirrors the
/// catalog's own version so the CLI surface and the VM source of truth bump
/// together.
pub const MCP_PRESETS_SCHEMA_VERSION: u32 = mcp_presets::PRESET_CATALOG_SCHEMA_VERSION;

pub(crate) fn run(args: &McpPresetsArgs) {
    if args.json {
        print!("{}", render_json());
    } else {
        print!("{}", render_text());
    }
}

/// Stable JSON envelope wrapping the full catalog.
pub fn render_json() -> String {
    let envelope: JsonEnvelope<PresetCatalog> =
        JsonEnvelope::ok(MCP_PRESETS_SCHEMA_VERSION, mcp_presets::catalog());
    let mut out = json_envelope::to_string_pretty(&envelope);
    out.push('\n');
    out
}

/// Human-readable listing: one block per preset.
pub fn render_text() -> String {
    use std::fmt::Write as _;

    let presets = mcp_presets::presets();
    let mut out = String::new();
    writeln!(
        out,
        "Well-known MCP server presets ({} total):",
        presets.len()
    )
    .ok();
    out.push('\n');
    for preset in presets {
        write_preset(&mut out, preset);
        out.push('\n');
    }
    out.push_str("Resolve a preset (fill in placeholders, log in if OAuth) then add it to\n");
    out.push_str("harn.toml as an MCP server. Run with --json for the machine-readable catalog.\n");
    out
}

fn write_preset(out: &mut String, preset: &McpPreset) {
    use std::fmt::Write as _;

    writeln!(out, "{}  ({})", preset.name, preset.id).ok();
    writeln!(out, "  {}", preset.description).ok();
    let transport = match preset.transport {
        mcp_presets::PresetTransport::Stdio => "stdio",
        mcp_presets::PresetTransport::Http => "http",
    };
    writeln!(out, "  transport: {transport}").ok();
    if preset.transport == mcp_presets::PresetTransport::Http {
        writeln!(out, "  url:       {}", preset.url).ok();
    } else {
        let argv = std::iter::once(preset.command.as_str())
            .chain(preset.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        writeln!(out, "  command:   {argv}").ok();
    }
    let auth = match preset.auth_kind {
        mcp_presets::PresetAuthKind::None => "none".to_string(),
        mcp_presets::PresetAuthKind::Oauth => match &preset.oauth_scopes {
            Some(scopes) => format!("oauth (scopes: {scopes})"),
            None => "oauth".to_string(),
        },
        mcp_presets::PresetAuthKind::ApiToken => "api_token".to_string(),
    };
    writeln!(out, "  auth:      {auth}").ok();
    if !preset.placeholders.is_empty() {
        let needs = preset
            .placeholders
            .iter()
            .map(|placeholder| {
                let req = if placeholder.required {
                    "required"
                } else {
                    "optional"
                };
                format!("{} ({req})", placeholder.label)
            })
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(out, "  needs:     {needs}").ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_envelope_round_trips() {
        let json = render_json();
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse presets json");
        assert_eq!(value["ok"], serde_json::json!(true));
        assert_eq!(
            value["schemaVersion"],
            serde_json::json!(MCP_PRESETS_SCHEMA_VERSION)
        );
        assert_eq!(
            value["data"]["schemaVersion"],
            serde_json::json!(MCP_PRESETS_SCHEMA_VERSION)
        );
        let presets = value["data"]["presets"].as_array().expect("presets array");
        assert_eq!(presets.len(), mcp_presets::presets().len());
        assert!(
            presets
                .iter()
                .any(|preset| preset["id"] == serde_json::json!("notion")),
            "notion preset should be present"
        );
    }

    #[test]
    fn text_lists_every_preset() {
        let text = render_text();
        for preset in mcp_presets::presets() {
            assert!(
                text.contains(&preset.name),
                "text catalog missing preset {}",
                preset.id
            );
        }
    }
}
