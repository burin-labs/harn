//! YAML frontmatter parsing for SKILL.md.
//!
//! Accepts the Anthropic / Claude-Code Agent Skills field set. Unknown
//! fields do not fail the load — they surface as warnings so newer
//! specifications can roll out without breaking older VM builds.
//!
//! Hyphenated and underscored field names are both accepted
//! (`when-to-use` == `when_to_use`, `disable-model-invocation` ==
//! `disable_model_invocation`, etc.) so authors can follow whichever
//! convention their docs prescribe.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_yaml_ng::Value as YamlValue;

/// Recognized SKILL.md frontmatter fields.
///
/// Matches the Anthropic Agent Skills spec plus Claude-Code-invented
/// extensions (`user-invocable`, `argument-hint`, `shell`, etc.). The
/// field names on the wire use hyphens; we accept both forms on parse.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillManifest {
    /// Required. Must match the enclosing `SKILL.md`'s directory name
    /// (case-sensitive) in a well-formed skill bundle.
    #[serde(default)]
    pub name: String,
    /// Required. Compact one-line card that says what the skill does
    /// and when to load it.
    #[serde(default)]
    pub short: String,
    /// One-line description surfaced to the model for auto-activation.
    #[serde(default)]
    pub description: String,
    /// Longer auto-activation trigger. Some specs call this `when-to-use`.
    #[serde(default)]
    pub when_to_use: Option<String>,
    /// If true, the skill is never auto-activated by the model — only
    /// explicit (`user-invocable` or direct-call) use.
    #[serde(default)]
    pub disable_model_invocation: bool,
    /// Restrict the tool set available while the skill is active.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// If true, users can trigger the skill via `/<skill-name>`.
    #[serde(default)]
    pub user_invocable: bool,
    /// Glob patterns of files the skill expects to touch. Used for
    /// host-side permission prompts and UI hints.
    #[serde(default)]
    pub paths: Vec<String>,
    /// `"fork"` means run in an isolated subcontext; left as a string
    /// so hosts can extend the set without a breaking enum change.
    #[serde(default)]
    pub context: Option<String>,
    /// Sub-agent this skill delegates to, if any.
    #[serde(default)]
    pub agent: Option<String>,
    /// Lifecycle hook commands keyed by event name
    /// (`on-activate`, `before-tool`, etc.).
    #[serde(default)]
    pub hooks: BTreeMap<String, String>,
    /// Preferred model alias.
    #[serde(default)]
    pub model: Option<String>,
    /// Effort hint (`low` / `medium` / `high`).
    #[serde(default)]
    pub effort: Option<String>,
    /// Require a cryptographic signature before `load_skill` will
    /// promote this skill into an agent session.
    #[serde(default)]
    pub require_signature: bool,
    /// Optional signer allowlist (SHA-256 fingerprints). When non-empty
    /// the signer must both be trusted locally and appear in this set.
    #[serde(default)]
    pub trusted_signers: Vec<String>,
    /// Optional endorsement signer allowlist. Required signed skills
    /// need at least one trusted endorsement, and when this list is
    /// non-empty every endorsement must come from one of these keys.
    #[serde(default)]
    pub trusted_endorsers: Vec<String>,
    /// Shell program to run the body under when `context == "shell"`.
    #[serde(default)]
    pub shell: Option<String>,
    /// User-facing template hint for `$ARGUMENTS`.
    #[serde(default)]
    pub argument_hint: Option<String>,
    /// Optional version/target constraint for version-aware grounding
    /// (e.g. `targets: zig >=0.16`). Harn does not interpret this — it is
    /// an opaque passthrough surfaced as skill metadata so hosts
    /// (#2965) can pick version-matched grounding cards.
    #[serde(default)]
    pub targets: Option<String>,
    /// MCP servers this skill declares. Each entry is an opaque spec object
    /// (the `{name, command/url, args, env, ...}` shape `__host_mcp_bootstrap`
    /// consumes). When the skill activates mid-conversation and the
    /// `mid_conversation_mcp_mount` loop flag is on, the agent loop mounts any
    /// of these servers not already active so their tools become visible and
    /// callable. Accepts `mcp` or `mcp-servers`/`mcp_servers` on the wire.
    #[serde(default, alias = "mcp_servers")]
    pub mcp: Vec<serde_json::Value>,
}

/// Outcome of parsing a SKILL.md frontmatter block.
#[derive(Debug, Clone)]
pub struct ParsedFrontmatter {
    pub manifest: SkillManifest,
    /// Names of keys present in the YAML but not mapped onto
    /// `SkillManifest`. Surfaced as warnings so future spec revisions
    /// roll out gracefully.
    pub unknown_fields: Vec<String>,
}

const KNOWN_CANONICAL_KEYS: &[&str] = &[
    "name",
    "short",
    "description",
    "when_to_use",
    "disable_model_invocation",
    "allowed_tools",
    "user_invocable",
    "paths",
    "context",
    "agent",
    "hooks",
    "model",
    "effort",
    "require_signature",
    "trusted_signers",
    "trusted_endorsers",
    "shell",
    "argument_hint",
    "targets",
    "mcp",
    "mcp_servers",
];

/// Split a SKILL.md file into (frontmatter_yaml, body).
///
/// A frontmatter block is an opening `---` line, arbitrary YAML, then a
/// closing `---` line, with everything before the opener discarded
/// (usually nothing, but a UTF-8 BOM is tolerated). If no frontmatter
/// is present, returns `("", full_source)`.
pub fn split_frontmatter(source: &str) -> (&str, &str) {
    let trimmed = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut line_start = 0usize;
    let mut saw_opener = false;
    let mut fm_start = 0usize;
    while line_start < trimmed.len() {
        let line_end = match trimmed[line_start..].find('\n') {
            Some(offset) => line_start + offset + 1,
            None => trimmed.len(),
        };
        let line = &trimmed[line_start..line_end];
        if !saw_opener {
            if line.trim().is_empty() {
                line_start = line_end;
                continue;
            }
            if line.trim() == "---" {
                saw_opener = true;
                fm_start = line_end;
                line_start = line_end;
                continue;
            }
            return ("", trimmed);
        }
        if line.trim() == "---" {
            return (&trimmed[fm_start..line_start], &trimmed[line_end..]);
        }
        line_start = line_end;
    }
    // Unterminated frontmatter: treat the whole file as body.
    ("", trimmed)
}

/// Parse a SKILL.md frontmatter block (YAML). Returns the populated
/// manifest plus any unknown keys (reported as warnings by callers).
pub fn parse_frontmatter(yaml: &str) -> Result<ParsedFrontmatter, String> {
    if yaml.trim().is_empty() {
        return Ok(ParsedFrontmatter {
            manifest: SkillManifest::default(),
            unknown_fields: Vec::new(),
        });
    }
    let raw: YamlValue =
        serde_yaml_ng::from_str(yaml).map_err(|e| format!("invalid SKILL.md YAML: {e}"))?;
    let map = match raw {
        YamlValue::Mapping(m) => m,
        YamlValue::Null => {
            return Ok(ParsedFrontmatter {
                manifest: SkillManifest::default(),
                unknown_fields: Vec::new(),
            });
        }
        other => {
            return Err(format!(
                "SKILL.md frontmatter must be a YAML mapping, got {:?}",
                discriminant(&other)
            ));
        }
    };

    // Normalize keys: hyphens -> underscores, strip surrounding whitespace.
    let mut normalized = serde_yaml_ng::Mapping::new();
    let mut unknown_fields = Vec::new();
    for (key, v) in map {
        let YamlValue::String(key_str) = key else {
            // Frontmatter keys are always strings; surface anything else as unknown.
            unknown_fields.push(format!("{key:?}"));
            continue;
        };
        let canonical = key_str.trim().replace('-', "_");
        if !KNOWN_CANONICAL_KEYS.contains(&canonical.as_str()) {
            unknown_fields.push(key_str);
            continue;
        }
        normalized.insert(YamlValue::String(canonical), v);
    }

    // Hooks sometimes arrive as a list of `{event: "...", command: "..."}`
    // entries rather than a map. Normalize both into a BTreeMap.
    if let Some(YamlValue::Sequence(seq)) = normalized.get("hooks").cloned() {
        let mut flat = serde_yaml_ng::Mapping::new();
        for item in seq {
            if let YamlValue::Mapping(entry) = item {
                let event = entry
                    .get("event")
                    .or_else(|| entry.get("name"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let cmd = entry
                    .get("command")
                    .or_else(|| entry.get("run"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                if let (Some(event), Some(cmd)) = (event, cmd) {
                    flat.insert(YamlValue::String(event), YamlValue::String(cmd));
                }
            }
        }
        normalized.insert(
            YamlValue::String("hooks".to_string()),
            YamlValue::Mapping(flat),
        );
    }

    let manifest: SkillManifest = serde_yaml_ng::from_value(YamlValue::Mapping(normalized))
        .map_err(|e| {
            format!(
                "SKILL.md frontmatter is well-formed YAML but doesn't match the expected field \
                 shapes: {e}"
            )
        })?;
    if !yaml.trim().is_empty() && manifest.short.trim().is_empty() {
        return Err("SKILL.md frontmatter requires a non-empty `short` field".to_string());
    }

    Ok(ParsedFrontmatter {
        manifest,
        unknown_fields,
    })
}

fn discriminant(value: &YamlValue) -> &'static str {
    match value {
        YamlValue::Null => "null",
        YamlValue::Bool(_) => "bool",
        YamlValue::Number(_) => "number",
        YamlValue::String(_) => "string",
        YamlValue::Sequence(_) => "sequence",
        YamlValue::Mapping(_) => "mapping",
        YamlValue::Tagged(_) => "tagged",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_frontmatter_and_body() {
        let src = "---\nname: hello\n---\n# Body\nline 2\n";
        let (fm, body) = split_frontmatter(src);
        assert_eq!(fm, "name: hello\n");
        assert_eq!(body, "# Body\nline 2\n");
    }

    #[test]
    fn no_frontmatter_returns_empty_and_full_body() {
        let src = "# Just body\nno fm here\n";
        let (fm, body) = split_frontmatter(src);
        assert!(fm.is_empty());
        assert_eq!(body, src);
    }

    #[test]
    fn tolerates_utf8_bom() {
        let src = "\u{feff}---\nname: hi\n---\nbody";
        let (fm, body) = split_frontmatter(src);
        assert_eq!(fm, "name: hi\n");
        assert_eq!(body, "body");
    }

    #[test]
    fn splits_crlf_frontmatter_without_truncating_yaml() {
        let src = concat!(
            "---\r\n",
            "name: manual\r\n",
            "short: Manual card\r\n",
            "disable-model-invocation: true\r\n",
            "---\r\n",
            "# Body\r\n",
        );
        let (fm, body) = split_frontmatter(src);

        assert_eq!(
            fm,
            "name: manual\r\nshort: Manual card\r\ndisable-model-invocation: true\r\n"
        );
        assert_eq!(body, "# Body\r\n");

        let parsed = parse_frontmatter(fm).expect("parse CRLF frontmatter");
        assert!(parsed.manifest.disable_model_invocation);
    }

    #[test]
    fn unterminated_frontmatter_becomes_body() {
        let src = "---\nname: hi\nno closing delim";
        let (fm, body) = split_frontmatter(src);
        assert!(fm.is_empty());
        assert_eq!(body, src);
    }

    #[test]
    fn parses_canonical_fields() {
        let yaml = "name: deploy\n\
                   short: \"Deploys the service when the user asks for a release\"\n\
                   description: \"Ship it\"\n\
                   when-to-use: \"when the user says deploy\"\n\
                   disable-model-invocation: true\n\
                   allowed-tools: [bash, git]\n\
                   user-invocable: true\n\
                   paths:\n  - infra/**\n  - Dockerfile\n\
                   model: claude-opus-4-7\n\
                   effort: high\n\
                   argument-hint: \"<target-env>\"\n";
        let parsed = parse_frontmatter(yaml).expect("parse");
        assert_eq!(parsed.manifest.name, "deploy");
        assert_eq!(
            parsed.manifest.short,
            "Deploys the service when the user asks for a release"
        );
        assert_eq!(parsed.manifest.description, "Ship it");
        assert_eq!(
            parsed.manifest.when_to_use.as_deref(),
            Some("when the user says deploy")
        );
        assert!(parsed.manifest.disable_model_invocation);
        assert!(parsed.manifest.user_invocable);
        assert_eq!(parsed.manifest.allowed_tools, vec!["bash", "git"]);
        assert_eq!(parsed.manifest.paths, vec!["infra/**", "Dockerfile"]);
        assert_eq!(parsed.manifest.model.as_deref(), Some("claude-opus-4-7"));
        assert_eq!(parsed.manifest.effort.as_deref(), Some("high"));
        assert_eq!(
            parsed.manifest.argument_hint.as_deref(),
            Some("<target-env>")
        );
        assert!(parsed.unknown_fields.is_empty());
    }

    #[test]
    fn targets_is_recognized_not_unknown() {
        // #2965: language SKILL.md cards carry a `targets:` field for
        // version-aware grounding. Harn must accept it (opaque passthrough)
        // and not flag it as an unknown frontmatter field.
        let yaml = "name: zig-grounding\nshort: Zig grounding card\ntargets: \"zig >=0.16\"\n";
        let parsed = parse_frontmatter(yaml).expect("parse");
        assert_eq!(parsed.manifest.targets.as_deref(), Some("zig >=0.16"));
        assert!(
            parsed.unknown_fields.is_empty(),
            "targets must not surface as an unknown field: {:?}",
            parsed.unknown_fields,
        );
    }

    #[test]
    fn mcp_servers_frontmatter_parses_and_is_recognized() {
        // A skill declares MCP servers to mount when it activates. The specs
        // are opaque `{name, command, args, ...}` objects preserved verbatim
        // for `__host_mcp_bootstrap`. `mcp-servers` is an accepted alias.
        let yaml = "name: weather\nshort: Weather lookups\n\
                    mcp-servers:\n  - name: weather-mcp\n    command: node\n    args: [\"server.js\"]\n";
        let parsed = parse_frontmatter(yaml).expect("parse");
        assert_eq!(parsed.manifest.mcp.len(), 1);
        let spec = &parsed.manifest.mcp[0];
        assert_eq!(
            spec.get("name").and_then(|v| v.as_str()),
            Some("weather-mcp")
        );
        assert_eq!(spec.get("command").and_then(|v| v.as_str()), Some("node"));
        assert!(
            parsed.unknown_fields.is_empty(),
            "mcp-servers must not surface as unknown: {:?}",
            parsed.unknown_fields,
        );
    }

    #[test]
    fn unknown_fields_surface_as_warnings_not_errors() {
        let yaml = "name: hi\nshort: Quick card\nfuture_field: future_value\n";
        let parsed = parse_frontmatter(yaml).expect("parse");
        assert_eq!(parsed.manifest.name, "hi");
        assert_eq!(parsed.unknown_fields, vec!["future_field"]);
    }

    #[test]
    fn hooks_as_mapping_or_sequence() {
        let mapping = "name: hi\nshort: Quick card\nhooks:\n  on-activate: \"echo up\"\n  on-deactivate: \"echo down\"\n";
        let parsed = parse_frontmatter(mapping).expect("parse mapping");
        assert_eq!(parsed.manifest.hooks.len(), 2);
        assert_eq!(
            parsed.manifest.hooks.get("on-activate").map(String::as_str),
            Some("echo up"),
        );

        let sequence = "name: hi\nshort: Quick card\nhooks:\n  - event: on-activate\n    command: \"echo up\"\n  - name: on-deactivate\n    run: \"echo down\"\n";
        let parsed = parse_frontmatter(sequence).expect("parse sequence");
        assert_eq!(
            parsed.manifest.hooks.get("on-activate").map(String::as_str),
            Some("echo up"),
        );
        assert_eq!(
            parsed
                .manifest
                .hooks
                .get("on-deactivate")
                .map(String::as_str),
            Some("echo down"),
        );
    }

    #[test]
    fn rejects_non_mapping_top_level() {
        let err = parse_frontmatter("- just\n- a list\n").unwrap_err();
        assert!(err.contains("mapping"), "{err}");
    }

    #[test]
    fn rejects_missing_short_field() {
        let err = parse_frontmatter("name: hi\n").unwrap_err();
        assert!(err.contains("`short`"), "{err}");
    }

    #[test]
    fn non_string_mapping_key_surfaces_as_unknown_not_panic() {
        // serde_yaml_ng (like upstream serde_yaml) is Value-keyed, so a
        // mapping key can be a number or bool rather than a string. The old
        // String-keyed YAML crate could never surface this shape, so the
        // key-extraction here is the semantic delta of the migration. Assert we
        // neither panic nor silently drop such keys:
        // they land in `unknown_fields` while the real string keys still parse.
        let yaml = "name: hi\nshort: Quick card\n123: numeric-key\ntrue: bool-key\n";
        let parsed = parse_frontmatter(yaml).expect("non-string keys must not error");
        assert_eq!(parsed.manifest.name, "hi");
        assert_eq!(parsed.manifest.short, "Quick card");
        assert_eq!(
            parsed.unknown_fields.len(),
            2,
            "both non-string keys must be surfaced: {:?}",
            parsed.unknown_fields,
        );
        assert!(
            parsed.unknown_fields.iter().any(|f| f.contains("123")),
            "numeric key must be surfaced: {:?}",
            parsed.unknown_fields,
        );
        assert!(
            parsed.unknown_fields.iter().any(|f| f.contains("true")),
            "boolean key must be surfaced: {:?}",
            parsed.unknown_fields,
        );
    }

    #[test]
    fn string_keyed_frontmatter_round_trips_after_value_keyed_migration() {
        // Guard the happy path across the String-keyed -> Value-keyed swap:
        // hyphenated canonical keys normalize, a hooks *list* still folds into
        // a map, and nothing spurious lands in `unknown_fields`.
        let yaml = "name: deploy\nshort: Ship it\ndisable-model-invocation: true\n\
                    hooks:\n  - event: on-activate\n    command: \"echo up\"\n";
        let parsed = parse_frontmatter(yaml).expect("string-keyed frontmatter must parse");
        assert_eq!(parsed.manifest.name, "deploy");
        assert!(parsed.manifest.disable_model_invocation);
        assert_eq!(
            parsed.manifest.hooks.get("on-activate").map(String::as_str),
            Some("echo up"),
        );
        assert!(
            parsed.unknown_fields.is_empty(),
            "canonical string keys must not surface as unknown: {:?}",
            parsed.unknown_fields,
        );
    }
}
