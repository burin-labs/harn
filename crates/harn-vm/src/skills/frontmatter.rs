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
use serde_yaml::Value as YamlValue;

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
    /// Shell program to run the body under when `context == "shell"`.
    #[serde(default)]
    pub shell: Option<String>,
    /// User-facing template hint for `$ARGUMENTS`.
    #[serde(default)]
    pub argument_hint: Option<String>,
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
    "shell",
    "argument_hint",
];

/// Split a SKILL.md file into (frontmatter_yaml, body).
///
/// A frontmatter block is an opening `---` line, arbitrary YAML, then a
/// closing `---` line, with everything before the opener discarded
/// (usually nothing, but a UTF-8 BOM is tolerated). If no frontmatter
/// is present, returns `("", full_source)`.
pub fn split_frontmatter(source: &str) -> (&str, &str) {
    let trimmed = source.strip_prefix('\u{feff}').unwrap_or(source);
    let leading_lines = trimmed.lines();
    let mut chars_consumed = 0usize;
    let mut saw_opener = false;
    let mut fm_start = 0usize;
    for line in leading_lines {
        let line_len_with_newline = line.len() + 1;
        if !saw_opener {
            if line.trim().is_empty() {
                chars_consumed += line_len_with_newline;
                continue;
            }
            if line.trim() == "---" {
                saw_opener = true;
                chars_consumed += line_len_with_newline;
                fm_start = chars_consumed;
                continue;
            }
            return ("", trimmed);
        }
        if line.trim() == "---" {
            let fm_end = chars_consumed;
            // Normalize body_start against the original &str length so we
            // never slice past the end when the file lacks a trailing \n.
            let body_start = (chars_consumed + line_len_with_newline).min(trimmed.len());
            return (&trimmed[fm_start..fm_end], &trimmed[body_start..]);
        }
        chars_consumed += line_len_with_newline;
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
        serde_yaml::from_str(yaml).map_err(|e| format!("invalid SKILL.md YAML: {e}"))?;
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
    let mut normalized = serde_yaml::Mapping::new();
    let mut unknown_fields = Vec::new();
    for (k, v) in map {
        let key_str = match k {
            YamlValue::String(s) => s,
            other => {
                return Err(format!(
                    "SKILL.md frontmatter keys must be strings, got {:?}",
                    discriminant(&other)
                ));
            }
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
        let mut flat = serde_yaml::Mapping::new();
        for item in seq {
            if let YamlValue::Mapping(entry) = item {
                let event = entry
                    .get(YamlValue::String("event".into()))
                    .or_else(|| entry.get(YamlValue::String("name".into())))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let cmd = entry
                    .get(YamlValue::String("command".into()))
                    .or_else(|| entry.get(YamlValue::String("run".into())))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                if let (Some(event), Some(cmd)) = (event, cmd) {
                    flat.insert(YamlValue::String(event), YamlValue::String(cmd));
                }
            }
        }
        normalized.insert(YamlValue::String("hooks".into()), YamlValue::Mapping(flat));
    }

    let manifest: SkillManifest =
        serde_yaml::from_value(YamlValue::Mapping(normalized)).map_err(|e| {
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
}
