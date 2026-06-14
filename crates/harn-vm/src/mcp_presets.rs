//! Canonical catalog of well-known MCP server presets (harn#2650).
//!
//! Thin clients (the burin-code TUI and the macOS GUI) used to each carry
//! their own hardcoded list of "one-click" MCP servers — Notion, Linear,
//! GitHub, a local filesystem server, etc. Those lists drifted from each
//! other. This module is the single harn-owned source of truth.
//!
//! **Data, not code (harn#3348).** The catalog ships as bundled TOML
//! (`mcp_presets.toml`, compiled in via `include_str!`) and is overlayable at
//! runtime without a recompile: set `HARN_MCP_PRESETS_CONFIG` to a TOML file,
//! or drop one at `~/.config/harn/mcp_presets.toml`. Overlays merge
//! last-writer-wins by `id`, then append new presets — mirroring how
//! `llm_config` layers `providers.toml`. On-disk fields are snake_case; the
//! serialized JSON contract (see [`PresetCatalog`]) stays camelCase so existing
//! consumers are byte-for-byte unaffected.
//!
//! The catalog is **descriptive metadata only** — it never connects to a
//! server or fabricates credentials. A preset is a template a client fills in
//! (allowed roots for filesystem, an OAuth login for Notion) before handing the
//! resolved spec to the MCP registry. Required substitutions are declared as
//! [`PresetPlaceholder`]s so a client can prompt for them.
//!
//! Bumping the serialized shape requires bumping [`PRESET_CATALOG_SCHEMA_VERSION`]
//! and coordinating consumers.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// JSON schema version for the preset catalog. Increment on any breaking
/// shape change to [`PresetCatalog`] / [`McpPreset`]. The optional
/// [`McpPreset::identity`] field added in harn#3348 is omitted when absent, so
/// the shipped output is unchanged and the version holds at 1 until a vetted
/// identity descriptor actually ships in the catalog.
pub const PRESET_CATALOG_SCHEMA_VERSION: u32 = 1;

/// Bundled default catalog. Editable here; overlayable at runtime.
const BUILTIN_TOML: &str = include_str!("mcp_presets.toml");

/// Transport a preset's server speaks. Mirrors the `transport` field of
/// [`crate::mcp::McpServerSpec`] so a resolved preset drops straight into a
/// server spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PresetTransport {
    /// Local subprocess speaking MCP over stdio.
    Stdio,
    /// Remote streamable-HTTP MCP endpoint.
    Http,
}

/// Hint about how a client authenticates to the server, so the UI can route
/// to the right setup affordance. Purely advisory — harn does not enforce it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetCategory {
    Productivity,
    Development,
    Local,
}

/// One value a client must collect before the preset can connect. The
/// `target` says where the resolved value goes (an env var, a CLI arg slot,
/// or the URL), and `token` is the literal token embedded in the template that
/// the client replaces.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub struct PresetPlaceholder {
    /// Stable identifier for the value (e.g. `"allowed_root"`).
    pub key: String,
    /// Human-readable label for a prompt (e.g. `"Allowed directory"`).
    pub label: String,
    /// Where the resolved value belongs.
    pub target: PlaceholderTarget,
    /// The literal token in the template to substitute, if any. `None` means
    /// the value is appended (e.g. a filesystem allowed-root positional arg).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Whether the preset cannot connect without this value.
    pub required: bool,
}

/// Where a [`PresetPlaceholder`] value is substituted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaceholderTarget {
    /// An environment variable named by the placeholder `key`.
    Env,
    /// A positional CLI argument (appended to `args`).
    Arg,
    /// Substituted into the `url` template.
    Url,
}

/// Declarative recipe for fetching a human-readable "logged in as …" string
/// for a server after auth (harn#3348 schema; the probe runner ships in
/// harn#3349). MCP has no standard `whoami`, so each known server needs a
/// vetted recipe. **Pure data** — defining it here changes no behavior until
/// the runner exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub struct IdentityProbeDescriptor {
    /// Display template referencing captured field names in braces, e.g.
    /// `"{name} <{email}> — {workspace}"`. The runner elides unresolved
    /// `{field}` placeholders (and any bracketed segment left empty).
    pub display_template: String,
    /// Ordered probe sources; the runner tries each until one yields a
    /// non-empty identity.
    #[serde(default)]
    pub sources: Vec<IdentityProbeSource>,
}

/// Where the identity runner looks. A flat struct (rather than a tagged enum)
/// keeps the TOML simple and dodges internally-tagged-enum/TOML edge cases;
/// the runner validates that the fields relevant to `kind` are present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub struct IdentityProbeSource {
    /// Which kind of probe this is.
    pub kind: IdentityProbeKind,
    /// MCP tool to call when `kind = tool` (e.g. Notion's self/whoami tool).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// HTTP endpoint to GET (with the bearer) when `kind = http`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// capture-name → dotted JSON path into the source's JSON payload, e.g.
    /// `name = "owner.user.name"`. Captures feed `display_template`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, String>,
}

/// The kind of identity probe a [`IdentityProbeSource`] performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityProbeKind {
    /// Capture fields from the OAuth token-exchange JSON response (Notion, for
    /// instance, returns `workspace_name` + `owner.user` inline).
    TokenResponse,
    /// Call a named MCP tool and capture fields from its JSON result.
    Tool,
    /// GET an authenticated HTTP endpoint and capture fields from its JSON.
    Http,
}

/// A single well-known MCP server preset. Fields after `transport` are
/// transport-specific: `command`/`args` populate a stdio spec, `url` populates
/// an HTTP spec. Empty strings mean "not applicable for this transport".
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub struct McpPreset {
    /// Stable lookup key (e.g. `"notion"`). Unique across the catalog.
    pub id: String,
    /// Display name for the client UI (e.g. `"Notion"`).
    pub name: String,
    /// One-line description of what the server exposes.
    pub description: String,
    /// SF Symbols-style icon hint for the macOS GUI; clients without an icon
    /// model may ignore it.
    pub icon: String,
    /// Advisory category for grouping.
    pub category: PresetCategory,
    /// Transport the resolved server speaks.
    pub transport: PresetTransport,
    /// stdio command (empty for HTTP presets).
    #[serde(default)]
    pub command: String,
    /// stdio command arguments (empty for HTTP presets).
    #[serde(default)]
    pub args: Vec<String>,
    /// HTTP endpoint URL template (empty for stdio presets).
    #[serde(default)]
    pub url: String,
    /// How a client authenticates.
    pub auth_kind: PresetAuthKind,
    /// Suggested OAuth scope string, when `auth_kind` is `oauth`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_scopes: Option<String>,
    /// Values the client must collect before connecting.
    #[serde(default)]
    pub placeholders: Vec<PresetPlaceholder>,
    /// Optional recipe for displaying the authenticated identity (harn#3348).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<IdentityProbeDescriptor>,
}

/// The full catalog, ready to serialize as the stable JSON contract.
#[derive(Debug, Clone, Serialize)]
pub struct PresetCatalog {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub presets: Vec<McpPreset>,
}

/// Deserialization envelope for a catalog TOML file (bundled or overlay).
#[derive(Debug, Default, Deserialize)]
struct PresetFile {
    #[serde(default)]
    presets: Vec<McpPreset>,
}

/// Lazily-built effective catalog (bundled base + runtime overlay).
static CATALOG: OnceLock<PresetCatalog> = OnceLock::new();

fn load() -> &'static PresetCatalog {
    CATALOG.get_or_init(build_catalog)
}

fn build_catalog() -> PresetCatalog {
    let mut presets = parse_presets(BUILTIN_TOML)
        .expect("embedded mcp_presets.toml must parse — invariant checked by tests");
    if let Some(overlay) = load_overlay() {
        merge_presets(&mut presets, overlay);
    }
    PresetCatalog {
        schema_version: PRESET_CATALOG_SCHEMA_VERSION,
        presets,
    }
}

/// Parse a catalog TOML document into its preset list.
fn parse_presets(src: &str) -> Result<Vec<McpPreset>, toml::de::Error> {
    Ok(toml::from_str::<PresetFile>(src)?.presets)
}

/// Resolve the runtime overlay, if any: the `HARN_MCP_PRESETS_CONFIG` path
/// wins, else `~/.config/harn/mcp_presets.toml`. Skipped under `cfg(test)` so
/// unit tests see only the bundled defaults plus explicit overlays.
fn load_overlay() -> Option<Vec<McpPreset>> {
    if let Ok(path) = std::env::var("HARN_MCP_PRESETS_CONFIG") {
        return read_overlay(&path);
    }
    if should_load_home_overlay() {
        let home = crate::user_dirs::home_dir()?;
        let path = home.join(".config").join("harn").join("mcp_presets.toml");
        return read_overlay(&path.to_string_lossy());
    }
    None
}

fn read_overlay(path: &str) -> Option<Vec<McpPreset>> {
    let content = std::fs::read_to_string(path).ok()?;
    match parse_presets(&content) {
        Ok(presets) => Some(presets),
        Err(error) => {
            eprintln!("[mcp_presets] TOML parse error in {path}: {error}");
            None
        }
    }
}

fn should_load_home_overlay() -> bool {
    !cfg!(test)
}

/// Merge an overlay into the base list: replace presets sharing an `id`
/// (last-writer-wins), append genuinely new ones in overlay order.
fn merge_presets(base: &mut Vec<McpPreset>, overlay: Vec<McpPreset>) {
    for preset in overlay {
        if let Some(existing) = base.iter_mut().find(|existing| existing.id == preset.id) {
            *existing = preset;
        } else {
            base.push(preset);
        }
    }
}

/// Borrow the effective preset list. The single source of truth.
pub fn presets() -> &'static [McpPreset] {
    load().presets.as_slice()
}

/// Look up one preset by its stable `id`.
pub fn preset(id: &str) -> Option<&'static McpPreset> {
    load().presets.iter().find(|preset| preset.id == id)
}

/// Build the serializable catalog envelope.
pub fn catalog() -> PresetCatalog {
    load().clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn base_presets() -> Vec<McpPreset> {
        parse_presets(BUILTIN_TOML).expect("bundled catalog parses")
    }

    #[test]
    fn bundled_catalog_parses() {
        let presets = base_presets();
        assert_eq!(presets.len(), 4, "bundled catalog should ship 4 presets");
    }

    #[test]
    fn catalog_carries_schema_version() {
        let catalog = catalog();
        assert_eq!(catalog.schema_version, PRESET_CATALOG_SCHEMA_VERSION);
        assert_eq!(catalog.presets.len(), presets().len());
    }

    #[test]
    fn preset_ids_are_unique() {
        let presets = base_presets();
        let ids: HashSet<&str> = presets.iter().map(|preset| preset.id.as_str()).collect();
        assert_eq!(ids.len(), presets.len(), "preset ids must be unique");
    }

    #[test]
    fn ships_the_well_known_servers() {
        for id in ["notion", "linear", "github", "filesystem"] {
            assert!(preset(id).is_some(), "missing preset {id}");
        }
    }

    #[test]
    fn transport_specific_fields_are_coherent() {
        for preset in base_presets() {
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
        for preset in base_presets() {
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
        assert!(
            notion.get("oauthScopes").is_none(),
            "Notion MCP does not currently expose configurable OAuth scopes"
        );
        assert!(
            notion.get("identity").is_none(),
            "no identity descriptor ships in the catalog yet (harn#3349 runner pending)"
        );
    }

    #[test]
    fn github_placeholder_round_trips_from_toml() {
        let github = base_presets()
            .into_iter()
            .find(|preset| preset.id == "github")
            .expect("github preset present");
        assert_eq!(github.placeholders.len(), 1);
        let placeholder = &github.placeholders[0];
        assert_eq!(placeholder.key, "GITHUB_PERSONAL_ACCESS_TOKEN");
        assert_eq!(placeholder.target, PlaceholderTarget::Env);
        assert!(placeholder.required);
        assert!(placeholder.token.is_none());
    }

    #[test]
    fn overlay_overrides_by_id_and_appends_new() {
        let mut base = base_presets();
        let overlay = parse_presets(
            r#"
[[presets]]
id = "notion"
name = "Notion (corp)"
description = "Corp Notion workspace."
icon = "doc.text.fill"
category = "productivity"
transport = "http"
url = "https://notion.corp.example/mcp"
auth_kind = "oauth"

[[presets]]
id = "sentry"
name = "Sentry"
description = "Errors and issues from Sentry."
icon = "exclamationmark.triangle.fill"
category = "development"
transport = "http"
url = "https://mcp.sentry.dev/mcp"
auth_kind = "oauth"
"#,
        )
        .expect("overlay parses");
        merge_presets(&mut base, overlay);

        let notion = base.iter().find(|preset| preset.id == "notion").unwrap();
        assert_eq!(notion.name, "Notion (corp)");
        assert_eq!(notion.url, "https://notion.corp.example/mcp");
        assert!(
            base.iter().any(|preset| preset.id == "sentry"),
            "new overlay preset should be appended"
        );
        assert_eq!(base.len(), 5, "4 base + 1 appended");
    }

    #[test]
    fn identity_descriptor_parses_from_toml() {
        let presets = parse_presets(
            r#"
[[presets]]
id = "notion"
name = "Notion"
description = "Notion workspace."
icon = "doc.text.fill"
category = "productivity"
transport = "http"
url = "https://mcp.notion.com/mcp"
auth_kind = "oauth"

[presets.identity]
display_template = "{name} <{email}> — {workspace}"

[[presets.identity.sources]]
kind = "token_response"
[presets.identity.sources.fields]
name = "owner.user.name"
email = "owner.user.person.email"
workspace = "workspace_name"

[[presets.identity.sources]]
kind = "tool"
tool = "notion-get-self"
[presets.identity.sources.fields]
name = "name"
email = "person.email"
"#,
        )
        .expect("identity descriptor parses");
        let identity = presets[0]
            .identity
            .as_ref()
            .expect("notion has identity descriptor");
        assert_eq!(identity.display_template, "{name} <{email}> — {workspace}");
        assert_eq!(identity.sources.len(), 2);
        assert_eq!(identity.sources[0].kind, IdentityProbeKind::TokenResponse);
        assert_eq!(
            identity.sources[0]
                .fields
                .get("workspace")
                .map(String::as_str),
            Some("workspace_name")
        );
        assert_eq!(identity.sources[1].kind, IdentityProbeKind::Tool);
        assert_eq!(identity.sources[1].tool.as_deref(), Some("notion-get-self"));

        // Round-trips to camelCase JSON for thin clients.
        let json = serde_json::to_value(&presets[0]).expect("serialize");
        assert_eq!(
            json["identity"]["displayTemplate"],
            serde_json::json!("{name} <{email}> — {workspace}")
        );
    }
}
