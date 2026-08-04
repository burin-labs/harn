//! MCP authenticated-identity resolution (harn#3349).
//!
//! MCP has no standard "who am I logged in as" call, so a connected server
//! can't tell a user *which* account/workspace they authorized. This module
//! turns the per-server [`IdentityProbeDescriptor`] recipes from the preset
//! catalog (harn#3348) into a human-readable "logged in as …" string.
//!
//! The headline source — `token_response` — needs no network: many providers
//! (Notion, for one) return workspace/owner fields right in the OAuth token
//! response, which the engine now captures onto
//! [`StoredMcpToken::token_response_extra`]. Resolving identity from that is a
//! pure, synchronous render — cheap enough to fold into `mcp status`. The
//! `tool` / `http` live-probe sources are recognized but resolved by a future
//! follow-up; this module returns `None` for them rather than guessing.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::mcp_oauth::StoredMcpToken;
use crate::mcp_presets::{
    self, IdentityProbeDescriptor, IdentityProbeKind, IdentityResolutionKind,
};

/// The identity descriptor for a server URL, from the preset catalog (including
/// any runtime overlay). `None` when the server isn't a known preset or the
/// preset declares no identity recipe.
pub fn descriptor_for(server_url: &str) -> Option<&'static IdentityProbeDescriptor> {
    mcp_presets::presets()
        .iter()
        .find(|preset| preset.url == server_url)?
        .identity
        .as_ref()
}

/// Resolve a display-identity string for a connected server from data already
/// on hand — the token-response payload captured at authorization. Sync, no
/// network. Returns `None` when there's no descriptor, no captured payload, or
/// nothing renders (e.g. the only sources are live `tool`/`http` probes).
pub fn display_identity(server_url: &str, token: &StoredMcpToken) -> Option<String> {
    let descriptor = descriptor_for(server_url)?;
    render_identity(descriptor, token.token_response_extra.as_ref())
}

/// Resolve a display-identity string from Harn's stored MCP OAuth token state.
///
/// This is the shared status-surface helper: callers get the same behavior in
/// CLI status, host status, and Harn scripts without each reimplementing OAuth
/// discovery, resource-indicator canonicalization, token lookup, and graceful
/// fallback. Returns `None` for unknown servers, servers without token-response
/// identity descriptors, missing tokens, discovery/storage errors, or tokens
/// that predate captured token-response extras.
pub async fn display_identity_from_store(
    server_url: &str,
    client_id_hint: Option<&str>,
) -> Option<String> {
    let descriptor = descriptor_for(server_url)?;
    if descriptor.resolution == IdentityResolutionKind::None
        || !descriptor
            .sources
            .iter()
            .any(|source| source.kind == IdentityProbeKind::TokenResponse)
    {
        return None;
    }
    let discovery = crate::mcp_oauth::discover(server_url).await.ok()?;
    let resource = crate::mcp_auth::canonical_resource_indicator(server_url).ok()?;
    let token = crate::mcp_oauth::load_token(
        &resource,
        &discovery.authorization_server_issuer,
        client_id_hint,
    )
    .await
    .ok()??;
    render_identity(descriptor, token.token_response_extra.as_ref())
}

/// Render a descriptor against an optional captured token-response payload.
/// Tries each `token_response` source in order; returns the first non-empty
/// render. Pure — the unit of behavior the tests exercise.
pub fn render_identity(
    descriptor: &IdentityProbeDescriptor,
    token_response: Option<&Value>,
) -> Option<String> {
    if descriptor.resolution == IdentityResolutionKind::None {
        return None;
    }
    for source in &descriptor.sources {
        if source.kind != IdentityProbeKind::TokenResponse {
            // `tool` / `http` need a live session; resolved by a follow-up.
            continue;
        }
        let Some(payload) = token_response else {
            continue;
        };
        let captures = capture(payload, &source.fields);
        if let Some(rendered) = render(&descriptor.display_template, &captures) {
            return Some(rendered);
        }
    }
    None
}

/// Capture each declared field (`name -> dotted.json.path`) from a payload.
/// Missing paths and non-scalar / empty values are simply absent from the map.
fn capture(payload: &Value, fields: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, path) in fields {
        if let Some(text) = lookup(payload, path).and_then(scalar_to_string) {
            if !text.is_empty() {
                out.insert(name.clone(), text);
            }
        }
    }
    out
}

/// Dotted-path lookup into a JSON value, e.g. `owner.user.person.email`.
fn lookup<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

fn scalar_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// Render `display_template` (e.g. `"{name} <{email}> — {workspace}"`) from the
/// captured fields. Present `{field}`s substitute; absent ones drop out, then
/// the result is tidied (empty `<>`/`()` removed, whitespace collapsed, dangling
/// separators trimmed). Returns `None` when no field resolved at all.
fn render(template: &str, captures: &BTreeMap<String, String>) -> Option<String> {
    let mut out = String::new();
    let mut any = false;
    for (index, part) in template.split('{').enumerate() {
        if index == 0 {
            out.push_str(part);
            continue;
        }
        match part.split_once('}') {
            Some((key, rest)) => {
                if let Some(value) = captures.get(key) {
                    out.push_str(value);
                    any = true;
                }
                out.push_str(rest);
            }
            // Unbalanced brace — treat literally.
            None => {
                out.push('{');
                out.push_str(part);
            }
        }
    }
    if !any {
        return None;
    }
    let tidied = tidy(&out);
    (!tidied.is_empty()).then_some(tidied)
}

/// Clean up a partially-substituted template: drop now-empty `<>`/`()` pairs,
/// collapse internal whitespace, and trim dangling separators left by missing
/// fields (but never trim `<`/`>` so a real `<email>` survives).
fn tidy(text: &str) -> String {
    let without_empty = text.replace("<>", "").replace("()", "");
    let collapsed = without_empty
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    collapsed
        .trim_matches(|c: char| c.is_whitespace() || matches!(c, '—' | '-' | '|' | ',' | ':'))
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_presets::{
        IdentityDescriptorConfidence, IdentityProbeKind, IdentityProbeSource,
    };
    use serde_json::json;

    fn notion_descriptor() -> IdentityProbeDescriptor {
        IdentityProbeDescriptor {
            resolution: IdentityResolutionKind::User,
            confidence: Some(IdentityDescriptorConfidence::Documented),
            source_url: Some("https://developers.notion.com/reference/create-a-token".to_string()),
            display_template: "{name} <{email}> — {workspace}".to_string(),
            sources: vec![IdentityProbeSource {
                kind: IdentityProbeKind::TokenResponse,
                tool: None,
                url: None,
                fields: BTreeMap::from([
                    ("name".to_string(), "owner.user.name".to_string()),
                    ("email".to_string(), "owner.user.person.email".to_string()),
                    ("workspace".to_string(), "workspace_name".to_string()),
                ]),
            }],
        }
    }

    #[test]
    fn lookup_walks_dotted_paths() {
        let payload = json!({"owner": {"user": {"name": "Jane"}}});
        assert_eq!(
            lookup(&payload, "owner.user.name").and_then(scalar_to_string),
            Some("Jane".to_string())
        );
        assert!(lookup(&payload, "owner.user.missing").is_none());
        assert!(lookup(&payload, "nope").is_none());
    }

    #[test]
    fn renders_full_identity() {
        let payload = json!({
            "workspace_name": "Acme",
            "owner": {"user": {"name": "Jane Doe", "person": {"email": "jane@acme.com"}}}
        });
        let rendered = render_identity(&notion_descriptor(), Some(&payload));
        assert_eq!(rendered.as_deref(), Some("Jane Doe <jane@acme.com> — Acme"));
    }

    #[test]
    fn elides_missing_trailing_field() {
        // No workspace_name → the " — {workspace}" tail drops cleanly.
        let payload = json!({
            "owner": {"user": {"name": "Jane Doe", "person": {"email": "jane@acme.com"}}}
        });
        let rendered = render_identity(&notion_descriptor(), Some(&payload));
        assert_eq!(rendered.as_deref(), Some("Jane Doe <jane@acme.com>"));
    }

    #[test]
    fn elides_missing_email_brackets() {
        // No email → the empty "<>" is removed, name + workspace remain.
        let payload = json!({"workspace_name": "Acme", "owner": {"user": {"name": "Jane"}}});
        let rendered = render_identity(&notion_descriptor(), Some(&payload));
        assert_eq!(rendered.as_deref(), Some("Jane — Acme"));
    }

    #[test]
    fn renders_only_workspace() {
        let payload = json!({"workspace_name": "Acme"});
        let rendered = render_identity(&notion_descriptor(), Some(&payload));
        assert_eq!(rendered.as_deref(), Some("Acme"));
    }

    #[test]
    fn none_when_no_fields_resolve() {
        let payload = json!({"unrelated": "x"});
        assert!(render_identity(&notion_descriptor(), Some(&payload)).is_none());
        assert!(render_identity(&notion_descriptor(), None).is_none());
    }

    #[test]
    fn skips_live_probe_only_sources() {
        let descriptor = IdentityProbeDescriptor {
            resolution: IdentityResolutionKind::User,
            confidence: Some(IdentityDescriptorConfidence::Observed),
            source_url: Some("https://example.com/mcp".to_string()),
            display_template: "{name}".to_string(),
            sources: vec![IdentityProbeSource {
                kind: IdentityProbeKind::Tool,
                tool: Some("whoami".to_string()),
                url: None,
                fields: BTreeMap::from([("name".to_string(), "name".to_string())]),
            }],
        };
        // Tool source isn't resolved synchronously yet → None even with payload.
        let payload = json!({"name": "Jane"});
        assert!(render_identity(&descriptor, Some(&payload)).is_none());
    }

    #[test]
    fn none_resolution_never_renders() {
        let descriptor = IdentityProbeDescriptor {
            resolution: IdentityResolutionKind::None,
            confidence: Some(IdentityDescriptorConfidence::None),
            source_url: Some("https://example.com/mcp".to_string()),
            display_template: "{name}".to_string(),
            sources: vec![IdentityProbeSource {
                kind: IdentityProbeKind::TokenResponse,
                tool: None,
                url: None,
                fields: BTreeMap::from([("name".to_string(), "name".to_string())]),
            }],
        };
        let payload = json!({"name": "Jane"});
        assert!(render_identity(&descriptor, Some(&payload)).is_none());
    }

    #[test]
    fn display_identity_uses_catalog_descriptor_for_notion() {
        // Exercises the catalog wiring end-to-end: the bundled Notion preset
        // ships a token_response descriptor (harn#3349), so a stored token whose
        // captured payload carries Notion's shape renders an identity.
        let token = StoredMcpToken {
            access_token: "a".into(),
            refresh_token: None,
            expires_at_unix: None,
            token_endpoint: "https://auth/token".into(),
            client_id: "c".into(),
            client_secret: None,
            token_endpoint_auth_method: "none".into(),
            issuer: "https://auth".into(),
            resource: "https://mcp.notion.com/mcp".into(),
            scopes: None,
            token_response_extra: Some(json!({
                "workspace_name": "Acme",
                "owner": {"user": {"name": "Jane Doe", "person": {"email": "jane@acme.com"}}}
            })),
        };
        assert_eq!(
            display_identity("https://mcp.notion.com/mcp", &token).as_deref(),
            Some("Jane Doe <jane@acme.com> — Acme")
        );
        // A server with no catalog descriptor yields nothing.
        let mut other = token;
        other.resource = "https://unknown.example/mcp".into();
        assert!(display_identity("https://unknown.example/mcp", &other).is_none());
    }

    #[tokio::test]
    async fn store_lookup_skips_unknown_servers_without_discovery() {
        let rendered =
            display_identity_from_store("https://identity.example.invalid/mcp", None).await;
        assert!(rendered.is_none());
    }
}
