//! Canonical catalog of well-known MCP server presets (harn#2650).
//!
//! Thin clients (the burin-code TUI and the macOS GUI) used to each carry
//! their own hardcoded list of "one-click" MCP servers — Notion, Linear,
//! GitHub, a local filesystem server, etc. Those lists drifted from each
//! other. This module is the single harn-owned source of truth: it ships a
//! static table of presets that any client renders identically.
//!
//! The catalog is **descriptive metadata only** — it never connects to a
//! server or fabricates credentials. A preset is a template a client fills
//! in (allowed roots for filesystem, an OAuth login for Notion) before
//! handing the resolved spec to the MCP registry. Required substitutions are
//! declared as [`PresetPlaceholder`]s so a client can prompt for them.
//!
//! The serialized JSON shape (see [`PresetCatalog`]) is a stable contract
//! surface consumed by downstream tooling. Bumping the shape requires
//! bumping [`PRESET_CATALOG_SCHEMA_VERSION`] and coordinating consumers.

use serde::Serialize;

/// JSON schema version for the preset catalog. Increment on any breaking
/// shape change to [`PresetCatalog`] / [`McpPreset`].
pub const PRESET_CATALOG_SCHEMA_VERSION: u32 = 1;

/// Transport a preset's server speaks. Mirrors the `transport` field of
/// [`crate::mcp::McpServerSpec`] so a resolved preset drops straight into a
/// server spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PresetTransport {
    /// Local subprocess speaking MCP over stdio.
    Stdio,
    /// Remote streamable-HTTP MCP endpoint.
    Http,
}

/// Hint about how a client authenticates to the server, so the UI can route
/// to the right setup affordance. Purely advisory — harn does not enforce it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetAuthKind {
    /// No credential needed (e.g. a local filesystem server).
    None,
    /// Interactive OAuth login (`harn mcp login`).
    Oauth,
    /// A static API token / personal access token supplied via env.
    ApiToken,
}

/// Loose grouping for client-side organization. Advisory; clients may ignore.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetCategory {
    Productivity,
    Development,
    Local,
}

/// One value a client must collect before the preset can connect. The
/// `target` says where the resolved value goes (an env var, a CLI arg slot,
/// or the URL), and `placeholder` is the literal token embedded in the
/// template that the client replaces.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetPlaceholder {
    /// Stable identifier for the value (e.g. `"allowed_root"`).
    pub key: &'static str,
    /// Human-readable label for a prompt (e.g. `"Allowed directory"`).
    pub label: &'static str,
    /// Where the resolved value belongs.
    pub target: PlaceholderTarget,
    /// The literal token in the template to substitute, if any. `None` means
    /// the value is appended (e.g. a filesystem allowed-root positional arg).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<&'static str>,
    /// Whether the preset cannot connect without this value.
    pub required: bool,
}

/// Where a [`PresetPlaceholder`] value is substituted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaceholderTarget {
    /// An environment variable named by the placeholder `key`.
    Env,
    /// A positional CLI argument (appended to `args`).
    Arg,
    /// Substituted into the `url` template.
    Url,
}

/// A single well-known MCP server preset. Fields after `transport` are
/// transport-specific: `command`/`args` populate a stdio spec, `url`
/// populates an HTTP spec. Empty strings mean "not applicable for this
/// transport".
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpPreset {
    /// Stable lookup key (e.g. `"notion"`). Unique across the catalog.
    pub id: &'static str,
    /// Display name for the client UI (e.g. `"Notion"`).
    pub name: &'static str,
    /// One-line description of what the server exposes.
    pub description: &'static str,
    /// SF Symbols-style icon hint for the macOS GUI; clients without an icon
    /// model may ignore it.
    pub icon: &'static str,
    /// Advisory category for grouping.
    pub category: PresetCategory,
    /// Transport the resolved server speaks.
    pub transport: PresetTransport,
    /// stdio command (empty for HTTP presets).
    pub command: &'static str,
    /// stdio command arguments (empty for HTTP presets).
    pub args: &'static [&'static str],
    /// HTTP endpoint URL template (empty for stdio presets).
    pub url: &'static str,
    /// How a client authenticates.
    pub auth_kind: PresetAuthKind,
    /// Suggested OAuth scope string, when `auth_kind` is `oauth`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth_scopes: Option<&'static str>,
    /// Values the client must collect before connecting.
    pub placeholders: &'static [PresetPlaceholder],
}

/// The full catalog, ready to serialize as the stable JSON contract.
#[derive(Debug, Clone, Serialize)]
pub struct PresetCatalog {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub presets: Vec<McpPreset>,
}

const NOTION_PLACEHOLDERS: &[PresetPlaceholder] = &[];

const LINEAR_PLACEHOLDERS: &[PresetPlaceholder] = &[];

const GITHUB_PLACEHOLDERS: &[PresetPlaceholder] = &[PresetPlaceholder {
    key: "GITHUB_PERSONAL_ACCESS_TOKEN",
    label: "GitHub personal access token",
    target: PlaceholderTarget::Env,
    token: None,
    required: true,
}];

const FILESYSTEM_PLACEHOLDERS: &[PresetPlaceholder] = &[PresetPlaceholder {
    key: "allowed_root",
    label: "Allowed directory",
    target: PlaceholderTarget::Arg,
    token: None,
    required: true,
}];

/// The canonical preset list. Order is the suggested display order.
const PRESETS: &[McpPreset] = &[
    McpPreset {
        id: "notion",
        name: "Notion",
        description: "Pages, databases, and comments from your Notion workspace.",
        icon: "doc.text.fill",
        category: PresetCategory::Productivity,
        transport: PresetTransport::Http,
        command: "",
        args: &[],
        url: "https://mcp.notion.com/mcp",
        auth_kind: PresetAuthKind::Oauth,
        oauth_scopes: Some("read write"),
        placeholders: NOTION_PLACEHOLDERS,
    },
    McpPreset {
        id: "linear",
        name: "Linear",
        description: "Issues, projects, and cycles from your Linear workspace.",
        icon: "list.bullet.rectangle.fill",
        category: PresetCategory::Productivity,
        transport: PresetTransport::Http,
        command: "",
        args: &[],
        url: "https://mcp.linear.app/mcp",
        auth_kind: PresetAuthKind::Oauth,
        oauth_scopes: Some("read write"),
        placeholders: LINEAR_PLACEHOLDERS,
    },
    McpPreset {
        id: "github",
        name: "GitHub",
        description: "Repositories, issues, and pull requests via the GitHub MCP server.",
        icon: "chevron.left.forwardslash.chevron.right",
        category: PresetCategory::Development,
        transport: PresetTransport::Stdio,
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-github"],
        url: "",
        auth_kind: PresetAuthKind::ApiToken,
        oauth_scopes: None,
        placeholders: GITHUB_PLACEHOLDERS,
    },
    McpPreset {
        id: "filesystem",
        name: "Filesystem",
        description: "Read and write files under one or more allowed local directories.",
        icon: "folder.fill",
        category: PresetCategory::Local,
        transport: PresetTransport::Stdio,
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-filesystem"],
        url: "",
        auth_kind: PresetAuthKind::None,
        oauth_scopes: None,
        placeholders: FILESYSTEM_PLACEHOLDERS,
    },
];

/// Borrow the static preset list. The single source of truth.
pub fn presets() -> &'static [McpPreset] {
    PRESETS
}

/// Look up one preset by its stable `id`.
pub fn preset(id: &str) -> Option<&'static McpPreset> {
    PRESETS.iter().find(|preset| preset.id == id)
}

/// Build the serializable catalog envelope.
pub fn catalog() -> PresetCatalog {
    PresetCatalog {
        schema_version: PRESET_CATALOG_SCHEMA_VERSION,
        presets: PRESETS.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn catalog_carries_schema_version() {
        let catalog = catalog();
        assert_eq!(catalog.schema_version, PRESET_CATALOG_SCHEMA_VERSION);
        assert_eq!(catalog.presets.len(), PRESETS.len());
    }

    #[test]
    fn preset_ids_are_unique() {
        let ids: HashSet<&str> = PRESETS.iter().map(|preset| preset.id).collect();
        assert_eq!(ids.len(), PRESETS.len(), "preset ids must be unique");
    }

    #[test]
    fn ships_the_well_known_servers() {
        for id in ["notion", "linear", "github", "filesystem"] {
            assert!(preset(id).is_some(), "missing preset {id}");
        }
    }

    #[test]
    fn transport_specific_fields_are_coherent() {
        for preset in PRESETS {
            match preset.transport {
                PresetTransport::Http => {
                    assert!(!preset.url.is_empty(), "{} http needs a url", preset.id);
                    assert!(
                        preset.command.is_empty(),
                        "{} http must not set a command",
                        preset.id
                    );
                }
                PresetTransport::Stdio => {
                    assert!(
                        !preset.command.is_empty(),
                        "{} stdio needs a command",
                        preset.id
                    );
                    assert!(
                        preset.url.is_empty(),
                        "{} stdio must not set a url",
                        preset.id
                    );
                }
            }
        }
    }

    #[test]
    fn oauth_scopes_only_on_oauth_presets() {
        for preset in PRESETS {
            if preset.oauth_scopes.is_some() {
                assert_eq!(
                    preset.auth_kind,
                    PresetAuthKind::Oauth,
                    "{} declares scopes but is not oauth",
                    preset.id
                );
            }
        }
    }

    #[test]
    fn json_shape_is_stable() {
        let json = serde_json::to_value(catalog()).expect("serialize catalog");
        assert_eq!(json["schemaVersion"], serde_json::json!(1));
        let notion = json["presets"]
            .as_array()
            .expect("presets array")
            .iter()
            .find(|preset| preset["id"] == serde_json::json!("notion"))
            .expect("notion preset present");
        assert_eq!(notion["transport"], serde_json::json!("http"));
        assert_eq!(notion["authKind"], serde_json::json!("oauth"));
        assert_eq!(
            notion["url"],
            serde_json::json!("https://mcp.notion.com/mcp")
        );
    }
}
