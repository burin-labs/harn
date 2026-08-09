//! Persisted MCP enable/disable allowlist + effective catalog (harn#2647).
//!
//! Part of the MCP comprehensiveness sub-epic: one
//! harn-owned MCP implementation, with thin clients (an IDE host's Rust
//! TUI and macOS GUI) that render toggle UIs **without storing any toggle
//! state of their own**. This module is the single source of truth for
//! which MCP **items** — tools, resources, and prompts — are enabled.
//!
//! ## Config shape
//!
//! The allowlist is a small, JSON-serializable document. The canonical
//! on-disk form is `mcp.json` (the burin clients keep it at
//! `~/.burin/mcp.json`) with an optional per-project overlay that layers on
//! top. harn-vm never hardcodes those paths — callers (harn-cli / the burin
//! host) pass file contents or paths in; this keeps the VM cross-platform
//! and free of client-specific layout assumptions.
//!
//! ```jsonc
//! {
//!   "schemaVersion": 1,
//!   "defaultEnabled": true,          // items absent from `items` use this
//!   "items": [
//!     { "server": "github", "kind": "tool",     "name": "create_issue", "enabled": false },
//!     { "server": "notion", "kind": "resource", "name": "page://root",  "enabled": true  },
//!     { "server": "notion", "kind": "prompt",   "name": "summarize",    "enabled": false }
//!   ]
//! }
//! ```
//!
//! An **overlay** merges over a **base**: the overlay's `defaultEnabled`
//! (when present) wins, and any overlay item replaces the matching base
//! item by its `(server, kind, name)` key. This lets a project pin a
//! narrower set than the user's global default.
//!
//! ## Effective catalog
//!
//! Given the live set of advertised items per server, [`build_catalog`]
//! projects the allowlist onto them to produce the **effective catalog**:
//! servers → items, each item carrying its resolved `enabled` flag. That
//! catalog is the stable contract a thin client renders as a toggle list;
//! the client reads it and never has to reconcile its own state.
//!
//! Both the [`McpAllowlist`] document and the [`McpCatalog`] projection are
//! versioned (see [`MCP_ALLOWLIST_SCHEMA_VERSION`]); bump the version on any
//! breaking shape change and coordinate consumers.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// JSON schema version shared by [`McpAllowlist`] and [`McpCatalog`].
/// Increment on any breaking shape change and coordinate consumers.
pub const MCP_ALLOWLIST_SCHEMA_VERSION: u32 = 1;

/// The three categories of item an MCP server can advertise. Mirrors the
/// MCP `tools/list`, `resources/list`, and `prompts/list` surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpItemKind {
    Tool,
    Resource,
    Prompt,
}

impl McpItemKind {
    /// Stable lowercase tag used in JSON and in dedup keys.
    pub fn as_str(self) -> &'static str {
        match self {
            McpItemKind::Tool => "tool",
            McpItemKind::Resource => "resource",
            McpItemKind::Prompt => "prompt",
        }
    }
}

/// One persisted enable/disable decision for a specific item. Items not
/// listed in an [`McpAllowlist`] fall back to its `default_enabled`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAllowlistItem {
    /// The owning MCP server's name (matches `harn.toml` `[[mcp]].name`).
    pub server: String,
    /// Which surface the item belongs to.
    pub kind: McpItemKind,
    /// The item's identifier within its server+kind: tool name, resource
    /// URI, or prompt name.
    pub name: String,
    /// Whether the item is enabled. `false` hides it from the agent.
    pub enabled: bool,
}

impl McpAllowlistItem {
    /// Stable identity key for dedup/merge: `(server, kind, name)`.
    fn key(&self) -> (String, McpItemKind, String) {
        (self.server.clone(), self.kind, self.name.clone())
    }
}

/// A persisted enable/disable allowlist covering tools, resources, and
/// prompts across every configured MCP server. This is the document a
/// client reads from / writes to disk; harn projects it into the effective
/// catalog and (eventually) enforces it at dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAllowlist {
    /// Schema version of this document.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// Enablement for any item not explicitly listed in `items`. A
    /// permissive default (`true`) means "allow everything except what is
    /// explicitly disabled"; `false` means "deny everything except what is
    /// explicitly enabled".
    #[serde(default = "default_true")]
    pub default_enabled: bool,
    /// Explicit per-item decisions. Order is not significant; the
    /// `(server, kind, name)` triple is the identity.
    #[serde(default)]
    pub items: Vec<McpAllowlistItem>,
}

fn default_schema_version() -> u32 {
    MCP_ALLOWLIST_SCHEMA_VERSION
}

fn default_true() -> bool {
    true
}

impl Default for McpAllowlist {
    fn default() -> Self {
        Self {
            schema_version: MCP_ALLOWLIST_SCHEMA_VERSION,
            default_enabled: true,
            items: Vec::new(),
        }
    }
}

impl McpAllowlist {
    /// Parse an allowlist from a JSON string. Unknown fields are ignored so
    /// older harn binaries can read documents written by newer clients.
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|error| format!("invalid mcp allowlist JSON: {error}"))
    }

    /// Serialize to a stable, pretty-printed JSON document.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Resolve the enablement of one item: an explicit entry wins, else the
    /// document default.
    pub fn is_enabled(&self, server: &str, kind: McpItemKind, name: &str) -> bool {
        self.items
            .iter()
            .find(|item| item.server == server && item.kind == kind && item.name == name)
            .map(|item| item.enabled)
            .unwrap_or(self.default_enabled)
    }

    /// Merge an overlay over `self` (the base), returning a new allowlist.
    ///
    /// - The overlay's `default_enabled` and `schema_version` win.
    /// - Each overlay item replaces the base item with the same
    ///   `(server, kind, name)` key; otherwise it is appended.
    /// - Base items the overlay does not mention are preserved.
    pub fn merge_overlay(&self, overlay: &McpAllowlist) -> McpAllowlist {
        let mut merged: BTreeMap<(String, McpItemKind, String), McpAllowlistItem> = self
            .items
            .iter()
            .map(|item| (item.key(), item.clone()))
            .collect();
        for item in &overlay.items {
            merged.insert(item.key(), item.clone());
        }
        McpAllowlist {
            schema_version: overlay.schema_version,
            default_enabled: overlay.default_enabled,
            items: merged.into_values().collect(),
        }
    }
}

/// One advertised item a server exposes, as seen by harn before the
/// allowlist is applied. The caller assembles these from live
/// `tools/list` / `resources/list` / `prompts/list` results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdvertisedItem {
    pub kind: McpItemKind,
    /// Tool name, resource URI, or prompt name.
    pub name: String,
    /// Optional human-readable label/title for the client UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Optional one-line description for the client UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The params shape for the `mcp/catalog` ACP request. A thin client (or
/// the burin host) supplies the persisted allowlist, an optional
/// per-project overlay, and the live advertised items per server; harn
/// merges + projects them into the effective catalog. Keeping the merge in
/// harn means clients store no toggle state of their own.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogRequest {
    /// Base allowlist (e.g. `~/.burin/mcp.json`). Absent → permissive
    /// defaults.
    #[serde(default)]
    pub allowlist: Option<McpAllowlist>,
    /// Optional per-project overlay merged over `allowlist`.
    #[serde(default)]
    pub overlay: Option<McpAllowlist>,
    /// Live advertised items per server name.
    #[serde(default)]
    pub advertised: BTreeMap<String, Vec<AdvertisedItem>>,
}

/// Build the effective catalog from a [`CatalogRequest`]. Merges the
/// optional overlay over the base allowlist (or permissive defaults) and
/// projects the result onto the advertised items.
pub fn catalog_for_request(request: &CatalogRequest) -> McpCatalog {
    let base = request.allowlist.clone().unwrap_or_default();
    let effective = match &request.overlay {
        Some(overlay) => base.merge_overlay(overlay),
        None => base,
    };
    build_catalog(&effective, &request.advertised)
}

/// One item in the effective catalog: an advertised item with its resolved
/// `enabled` flag from the allowlist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCatalogItem {
    pub kind: McpItemKind,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub enabled: bool,
}

/// One server's slice of the effective catalog: its advertised items, each
/// with its resolved enablement. Items are sorted by `(kind, name)` for
/// stable diffs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCatalogServer {
    pub name: String,
    pub items: Vec<McpCatalogItem>,
}

/// The effective catalog: the stable JSON contract a thin client renders as
/// a toggle UI. Servers are sorted by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCatalog {
    pub schema_version: u32,
    /// Effective document default applied to items with no explicit entry.
    pub default_enabled: bool,
    pub servers: Vec<McpCatalogServer>,
}

/// Project `allowlist` onto the per-server advertised items to build the
/// effective catalog. Servers and items are sorted for stable output.
pub fn build_catalog(
    allowlist: &McpAllowlist,
    advertised: &BTreeMap<String, Vec<AdvertisedItem>>,
) -> McpCatalog {
    let mut servers = Vec::with_capacity(advertised.len());
    for (server_name, items) in advertised {
        let mut catalog_items: Vec<McpCatalogItem> = items
            .iter()
            .map(|item| McpCatalogItem {
                kind: item.kind,
                name: item.name.clone(),
                title: item.title.clone(),
                description: item.description.clone(),
                enabled: allowlist.is_enabled(server_name, item.kind, &item.name),
            })
            .collect();
        catalog_items
            .sort_by(|left, right| (left.kind, &left.name).cmp(&(right.kind, &right.name)));
        servers.push(McpCatalogServer {
            name: server_name.clone(),
            items: catalog_items,
        });
    }
    servers.sort_by(|left, right| left.name.cmp(&right.name));
    McpCatalog {
        schema_version: MCP_ALLOWLIST_SCHEMA_VERSION,
        default_enabled: allowlist.default_enabled,
        servers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(server: &str, kind: McpItemKind, name: &str, enabled: bool) -> McpAllowlistItem {
        McpAllowlistItem {
            server: server.to_string(),
            kind,
            name: name.to_string(),
            enabled,
        }
    }

    #[test]
    fn default_is_permissive() {
        let allowlist = McpAllowlist::default();
        assert_eq!(allowlist.schema_version, MCP_ALLOWLIST_SCHEMA_VERSION);
        assert!(allowlist.default_enabled);
        assert!(allowlist.items.is_empty());
        // Unlisted items follow the default.
        assert!(allowlist.is_enabled("github", McpItemKind::Tool, "anything"));
    }

    #[test]
    fn explicit_item_overrides_default() {
        let allowlist = McpAllowlist {
            schema_version: 1,
            default_enabled: true,
            items: vec![item("github", McpItemKind::Tool, "create_issue", false)],
        };
        assert!(!allowlist.is_enabled("github", McpItemKind::Tool, "create_issue"));
        // A different name still follows the default.
        assert!(allowlist.is_enabled("github", McpItemKind::Tool, "list_issues"));
        // Same name, different kind is a different item.
        assert!(allowlist.is_enabled("github", McpItemKind::Resource, "create_issue"));
    }

    #[test]
    fn deny_by_default_requires_explicit_enable() {
        let allowlist = McpAllowlist {
            schema_version: 1,
            default_enabled: false,
            items: vec![item("notion", McpItemKind::Prompt, "summarize", true)],
        };
        assert!(allowlist.is_enabled("notion", McpItemKind::Prompt, "summarize"));
        assert!(!allowlist.is_enabled("notion", McpItemKind::Prompt, "other"));
    }

    #[test]
    fn roundtrips_through_json() {
        let allowlist = McpAllowlist {
            schema_version: 1,
            default_enabled: false,
            items: vec![
                item("github", McpItemKind::Tool, "create_issue", false),
                item("notion", McpItemKind::Resource, "page://root", true),
            ],
        };
        let json = allowlist.to_json();
        let parsed = McpAllowlist::from_json(&json).expect("parse");
        assert_eq!(parsed, allowlist);
    }

    #[test]
    fn parses_minimal_document_with_defaults() {
        // No defaultEnabled / schemaVersion → serde defaults apply.
        let parsed = McpAllowlist::from_json("{}").expect("parse");
        assert_eq!(parsed.schema_version, MCP_ALLOWLIST_SCHEMA_VERSION);
        assert!(parsed.default_enabled);
        assert!(parsed.items.is_empty());
    }

    #[test]
    fn ignores_unknown_fields() {
        // Forward-compat: a newer client may add fields harn does not know.
        let json = r#"{ "schemaVersion": 1, "futureField": 42, "items": [] }"#;
        let parsed = McpAllowlist::from_json(json).expect("parse");
        assert!(parsed.items.is_empty());
    }

    #[test]
    fn overlay_replaces_matching_items_and_wins_default() {
        let base = McpAllowlist {
            schema_version: 1,
            default_enabled: true,
            items: vec![
                item("github", McpItemKind::Tool, "create_issue", true),
                item("github", McpItemKind::Tool, "delete_repo", false),
            ],
        };
        let overlay = McpAllowlist {
            schema_version: 1,
            default_enabled: false,
            // Re-enable delete_repo for this project, leave create_issue.
            items: vec![item("github", McpItemKind::Tool, "delete_repo", true)],
        };
        let merged = base.merge_overlay(&overlay);
        // Overlay default wins.
        assert!(!merged.default_enabled);
        // Overlay item replaces the base decision.
        assert!(merged.is_enabled("github", McpItemKind::Tool, "delete_repo"));
        // Base item the overlay didn't mention is preserved.
        assert!(merged.is_enabled("github", McpItemKind::Tool, "create_issue"));
        assert_eq!(merged.items.len(), 2);
    }

    fn advertised() -> BTreeMap<String, Vec<AdvertisedItem>> {
        let mut map = BTreeMap::new();
        map.insert(
            "notion".to_string(),
            vec![AdvertisedItem {
                kind: McpItemKind::Prompt,
                name: "summarize".to_string(),
                title: Some("Summarize".to_string()),
                description: None,
            }],
        );
        map.insert(
            "github".to_string(),
            vec![
                AdvertisedItem {
                    kind: McpItemKind::Tool,
                    name: "list_issues".to_string(),
                    title: None,
                    description: Some("List issues".to_string()),
                },
                AdvertisedItem {
                    kind: McpItemKind::Tool,
                    name: "create_issue".to_string(),
                    title: None,
                    description: None,
                },
            ],
        );
        map
    }

    #[test]
    fn catalog_applies_allowlist_and_sorts() {
        let allowlist = McpAllowlist {
            schema_version: 1,
            default_enabled: true,
            items: vec![item("github", McpItemKind::Tool, "create_issue", false)],
        };
        let catalog = build_catalog(&allowlist, &advertised());
        assert_eq!(catalog.schema_version, MCP_ALLOWLIST_SCHEMA_VERSION);
        assert!(catalog.default_enabled);
        // Servers sorted by name: github before notion.
        assert_eq!(catalog.servers[0].name, "github");
        assert_eq!(catalog.servers[1].name, "notion");
        // github items sorted by (kind, name): create_issue before list_issues.
        let github = &catalog.servers[0];
        assert_eq!(github.items[0].name, "create_issue");
        assert!(!github.items[0].enabled);
        assert_eq!(github.items[1].name, "list_issues");
        assert!(github.items[1].enabled);
        // notion prompt follows default (enabled).
        assert!(catalog.servers[1].items[0].enabled);
    }

    #[test]
    fn catalog_for_request_merges_overlay_then_projects() {
        let json = serde_json::json!({
            "allowlist": { "schemaVersion": 1, "defaultEnabled": true, "items": [
                { "server": "github", "kind": "tool", "name": "create_issue", "enabled": false }
            ] },
            "overlay": { "schemaVersion": 1, "defaultEnabled": true, "items": [
                { "server": "github", "kind": "tool", "name": "create_issue", "enabled": true }
            ] },
            "advertised": {
                "github": [ { "kind": "tool", "name": "create_issue" } ]
            }
        });
        let request: CatalogRequest = serde_json::from_value(json).expect("parse request");
        let catalog = catalog_for_request(&request);
        // Overlay re-enabled create_issue.
        assert!(catalog.servers[0].items[0].enabled);
    }

    #[test]
    fn catalog_for_request_uses_permissive_defaults_when_absent() {
        let request = CatalogRequest {
            allowlist: None,
            overlay: None,
            advertised: advertised(),
        };
        let catalog = catalog_for_request(&request);
        assert!(catalog.default_enabled);
        // All items enabled under the permissive default.
        for server in &catalog.servers {
            for item in &server.items {
                assert!(item.enabled);
            }
        }
    }

    #[test]
    fn catalog_json_shape_is_stable() {
        let allowlist = McpAllowlist {
            schema_version: 1,
            default_enabled: true,
            items: vec![item("github", McpItemKind::Tool, "create_issue", false)],
        };
        let catalog = build_catalog(&allowlist, &advertised());
        let value = serde_json::to_value(&catalog).expect("serialize");
        assert_eq!(value["schemaVersion"], serde_json::json!(1));
        assert_eq!(value["defaultEnabled"], serde_json::json!(true));
        let github = &value["servers"][0];
        assert_eq!(github["name"], serde_json::json!("github"));
        let create_issue = &github["items"][0];
        assert_eq!(create_issue["kind"], serde_json::json!("tool"));
        assert_eq!(create_issue["name"], serde_json::json!("create_issue"));
        assert_eq!(create_issue["enabled"], serde_json::json!(false));
        // Optional fields omitted when absent.
        assert!(create_issue.get("title").is_none());
    }
}
