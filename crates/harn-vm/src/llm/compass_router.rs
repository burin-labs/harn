//! Burin compass tool-rewrite router (B.9, #2612).
//!
//! The compass *steer* (#2521, the `compass_ast_edits` reminder provider)
//! tells the agent at session start to prefer the AST-precise edit
//! primitives. This module is the *active routing layer*: a per-tool-call
//! hook that observes a freeform / whole-file edit call before it is
//! dispatched and, conservatively, either
//!
//! - **suggests** the structural primitive it should have reached for
//!   (advisory; the original call still runs), or
//! - **rewrites** the call into a structural / hash-guarded form when the
//!   substitution is provably equivalent (same text transform), falling
//!   back to the original call otherwise.
//!
//! It sits in [`crate::llm::agent_host_primitives::host_agent_dispatch_tool_call`],
//! after permission / pre-tool hooks and schema validation but *before*
//! [`crate::llm::agent_tools::dispatch_tool_execution_with_mcp`] — the
//! single per-tool-call chokepoint every agent surface (TUI, IDE,
//! cloud-supervised) funnels through. The hook is additive: when the
//! compass is disabled, or the call is not a freeform edit, [`route`]
//! returns [`CompassDecision::Passthrough`] and the dispatcher behaves
//! exactly as before.
//!
//! ## Default + disable
//!
//! On by default in `suggest` mode. A session/persona controls it through
//! the `compass` option:
//!
//! - `compass: false` (or `compass: {enabled: false}`) — fully off.
//! - `compass: {mode: "off"}` — registered but inert (same as disabled).
//! - `compass: {mode: "suggest"}` — default; advisory only.
//! - `compass: {mode: "rewrite"}` — silently substitute provably-equivalent
//!   structural calls; suggest (and fall back) otherwise.
//! - `compass: {prefer: ["edit_rename_symbol", ...]}` — per-persona
//!   ordering hint, consumed from `personas/fixer/manifest.harn`'s
//!   `edit_strategy.prefer` signal.
//!
//! ## Observability (A.10)
//!
//! Each decision increments a `harn.compass.*` counter via
//! [`crate::stdlib::observability::emit_instrument`], tagged with the
//! persona and the freeform/structural tool names:
//!
//! - `harn.compass.suggested` — advisory routing decision emitted.
//! - `harn.compass.rewritten` — call silently substituted.
//! - `harn.compass.fell_back` — rewrite considered but not provably
//!   equivalent, so the original freeform call ran.

use std::collections::BTreeMap;

use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::stdlib::observability::{emit_instrument, MetricInstrument};

/// Counter names live under their own `harn.compass.*` namespace
/// (declared in [`crate::observability::vocabulary::COMPASS`]).
const COUNTER_SUGGESTED: &str = "harn.compass.suggested";
const COUNTER_REWRITTEN: &str = "harn.compass.rewritten";
const COUNTER_FELL_BACK: &str = "harn.compass.fell_back";

const DEFAULT_PERSONA: &str = "default";

/// How aggressively the compass acts on a freeform-edit call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompassMode {
    /// Registered but inert (equivalent to disabled).
    Off,
    /// Emit an advisory routing-decision reminder; never change bytes.
    Suggest,
    /// Silently substitute a provably-equivalent structural call;
    /// fall back to `Suggest` semantics when not provable.
    Rewrite,
}

impl CompassMode {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" | "disabled" | "none" => Some(Self::Off),
            "suggest" | "advise" | "advisory" => Some(Self::Suggest),
            "rewrite" | "auto" => Some(Self::Rewrite),
            _ => None,
        }
    }
}

/// Resolved compass configuration for a single tool call.
#[derive(Clone, Debug)]
pub(crate) struct CompassConfig {
    pub mode: CompassMode,
    pub persona: String,
    /// Persona-declared ordering of structural primitives
    /// (`edit_strategy.prefer`). Influences which suggestion the router
    /// surfaces when several structural targets fit.
    pub prefer: Vec<String>,
}

impl CompassConfig {
    /// Read the `compass` option out of the agent-loop options dict
    /// (already JSON-shaped). Defaults to on/`suggest` so the compass is
    /// the agent-loop default per #2521; the caller short-circuits when
    /// [`CompassMode::Off`].
    pub(crate) fn from_options(options: &JsonValue) -> Self {
        let persona = options
            .get("persona")
            .and_then(JsonValue::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_PERSONA)
            .to_string();
        let compass = options.get("compass");
        let mode = match compass {
            // Absent → default on (suggest).
            None | Some(JsonValue::Null) => CompassMode::Suggest,
            // `compass: false` fully disables; `compass: true` is on.
            Some(JsonValue::Bool(false)) => CompassMode::Off,
            Some(JsonValue::Bool(true)) => CompassMode::Suggest,
            Some(JsonValue::Object(map)) => {
                if map
                    .get("enabled")
                    .and_then(JsonValue::as_bool)
                    .is_some_and(|enabled| !enabled)
                {
                    CompassMode::Off
                } else {
                    map.get("mode")
                        .and_then(JsonValue::as_str)
                        .and_then(CompassMode::parse)
                        .unwrap_or(CompassMode::Suggest)
                }
            }
            // Unknown shape → keep the safe default rather than erroring
            // inside the dispatch hot path.
            Some(_) => CompassMode::Suggest,
        };
        let prefer = compass
            .and_then(|value| value.get("prefer"))
            .or_else(|| {
                options
                    .get("edit_strategy")
                    .and_then(|value| value.get("prefer"))
            })
            .and_then(JsonValue::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(JsonValue::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            mode,
            persona,
            prefer,
        }
    }
}

/// Which structural primitive a freeform edit should reach for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StructuralTarget {
    /// Node-level replacement (`edit_apply_node`).
    ApplyNode,
    /// Cross-file safe rename (`edit_rename_symbol`).
    RenameSymbol,
    /// Hash-guarded, atomic single/multi-hunk patch (`edit_safe_text_patch`).
    /// Same text transform as a raw `str_replace`, but with a stale-base
    /// guard and staged-fs atomicity — the provably-equivalent rewrite the
    /// router can perform without touching disk.
    SafeTextPatch,
}

impl StructuralTarget {
    pub(crate) fn tool_name(self) -> &'static str {
        match self {
            Self::ApplyNode => "edit_apply_node",
            Self::RenameSymbol => "edit_rename_symbol",
            Self::SafeTextPatch => "edit_safe_text_patch",
        }
    }
}

/// The router's decision for one tool call.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CompassDecision {
    /// Not a freeform edit, or compass disabled — dispatch unchanged.
    Passthrough,
    /// Advisory: surface this reminder body, then dispatch the original
    /// call unchanged. Carries the counter context.
    Suggest {
        target: StructuralTarget,
        reminder_body: String,
    },
    /// Provably-equivalent substitution: dispatch this tool with these
    /// args instead of the original.
    Rewrite {
        target: StructuralTarget,
        tool_name: String,
        tool_args: JsonValue,
    },
}

/// A freeform / whole-file edit call the compass recognises, normalised
/// into the fields the router reasons about.
#[derive(Clone, Debug)]
struct FreeformEdit {
    /// Resolved path of the file being edited, if present.
    path: Option<String>,
    /// `(old_text, new_text)` hunks the edit applies, when the call is a
    /// text-replace shape. Empty for whole-file writes.
    hunks: Vec<(String, String)>,
    /// True for a whole-file content write (`write_file` / `create_file`).
    whole_file: bool,
}

/// File extensions Harn has structural (tree-sitter) support for. The
/// router only steers edits on these — for everything else the structural
/// tools degrade to `Unsupported`, so a freeform patch is the right call
/// and the compass stays out of the way. Kept conservative on purpose;
/// `edit_capabilities()` is the authority at runtime, but a static allow
/// list keeps this hook free of I/O.
fn parseable_extension(path: &str) -> bool {
    const PARSEABLE: &[&str] = &[
        "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "java", "kt", "kts", "rb", "c",
        "h", "cc", "cpp", "hpp", "cs", "swift", "scala", "php", "lua", "harn",
    ];
    path.rsplit('.')
        .next()
        .map(|ext| PARSEABLE.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Recognise a freeform-edit tool call by name + arg shape. Conservative:
/// returns `None` for anything that is already structural, or whose shape
/// we don't understand, so the router leaves it untouched.
fn classify_freeform_edit(tool_name: &str, args: &JsonValue) -> Option<FreeformEdit> {
    let normalized = tool_name.trim().to_ascii_lowercase();
    // Already structural — never re-route a structural call onto itself.
    if normalized.starts_with("edit_apply_node")
        || normalized.starts_with("edit_insert_at_anchor")
        || normalized.starts_with("edit_rename_symbol")
        || normalized.starts_with("edit_dry_run")
        || normalized.starts_with("edit_extract")
        || normalized.starts_with("edit_change_signature")
        || normalized.starts_with("edit_inline")
        || normalized.starts_with("edit_move")
    {
        return None;
    }

    let path = args
        .get("path")
        .or_else(|| args.get("file"))
        .or_else(|| args.get("file_path"))
        .and_then(JsonValue::as_str)
        .map(str::to_string);

    // str_replace-shape: {path, old_text/old_str/old, new_text/new_str/new}
    let is_replace_name = matches!(
        normalized.as_str(),
        "str_replace" | "str_replace_editor" | "apply_patch" | "edit_file" | "replace_in_file"
    );
    if is_replace_name {
        let old_text = first_str(args, &["old_text", "old_str", "old", "search"]);
        let new_text = first_str(args, &["new_text", "new_str", "new", "replace"]);
        if let (Some(old_text), Some(new_text)) = (old_text, new_text) {
            return Some(FreeformEdit {
                path,
                hunks: vec![(old_text, new_text)],
                whole_file: false,
            });
        }
        // Hunks-array shape: {path, hunks: [{old_text, new_text}, ...]}
        if let Some(hunks) = extract_hunks(args) {
            return Some(FreeformEdit {
                path,
                hunks,
                whole_file: false,
            });
        }
    }

    // edit_safe_text_patch is itself the hash-guarded target; only steer it
    // when its single hunk reads like a rename (toward edit_rename_symbol).
    if normalized == "edit_safe_text_patch" {
        let hunks = extract_hunks(args)?;
        return Some(FreeformEdit {
            path,
            hunks,
            whole_file: false,
        });
    }

    // Whole-file write of an existing source file.
    let is_write_name = matches!(
        normalized.as_str(),
        "write_file" | "create_file" | "write" | "save_file"
    );
    if is_write_name {
        let has_content = args.get("content").is_some() || args.get("contents").is_some();
        if has_content {
            return Some(FreeformEdit {
                path,
                hunks: Vec::new(),
                whole_file: true,
            });
        }
    }

    None
}

fn first_str(args: &JsonValue, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| args.get(*key).and_then(JsonValue::as_str))
        .map(str::to_string)
}

fn extract_hunks(args: &JsonValue) -> Option<Vec<(String, String)>> {
    let arr = args.get("hunks")?.as_array()?;
    let mut hunks = Vec::with_capacity(arr.len());
    for hunk in arr {
        let old_text = first_str(hunk, &["old_text", "old_str", "old", "search"])?;
        let new_text = first_str(hunk, &["new_text", "new_str", "new", "replace"])?;
        hunks.push((old_text, new_text));
    }
    if hunks.is_empty() {
        None
    } else {
        Some(hunks)
    }
}

/// If a single hunk renames exactly one identifier token (old/new are both
/// bare identifiers and differ), return `(old_ident, new_ident)`. Used to
/// *suggest* `edit_rename_symbol` — never to rewrite, because a single-hunk
/// textual replace is not byte-equivalent to a project-wide rename.
fn single_token_rename(hunks: &[(String, String)]) -> Option<(String, String)> {
    if hunks.len() != 1 {
        return None;
    }
    let (old_text, new_text) = &hunks[0];
    let old_trim = old_text.trim();
    let new_trim = new_text.trim();
    if old_trim == new_trim || !is_bare_identifier(old_trim) || !is_bare_identifier(new_trim) {
        return None;
    }
    Some((old_trim.to_string(), new_trim.to_string()))
}

fn is_bare_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Build the `edit_safe_text_patch` args that reproduce a raw text-replace
/// call exactly: same path, same hunks. This is the one rewrite the router
/// can prove equivalent without disk I/O — `edit_safe_text_patch` applies
/// the identical `old_text -> new_text` matcher, but adds a stale-base hash
/// guard and routes through staged-fs for atomicity. Returns `None` when
/// any field needed to reproduce the call faithfully is missing.
fn rewrite_to_safe_text_patch(edit: &FreeformEdit, original: &JsonValue) -> Option<JsonValue> {
    let path = edit.path.as_ref()?;
    if edit.whole_file || edit.hunks.is_empty() {
        return None;
    }
    let hunks: Vec<JsonValue> = edit
        .hunks
        .iter()
        .map(|(old_text, new_text)| {
            serde_json::json!({ "old_text": old_text, "new_text": new_text })
        })
        .collect();
    let mut out = JsonMap::new();
    out.insert("path".to_string(), JsonValue::String(path.clone()));
    out.insert("hunks".to_string(), JsonValue::Array(hunks));
    // Preserve a caller-supplied session_id so the rewrite stays inside the
    // same staged-fs transaction as its siblings.
    for passthrough in ["session_id", "match_options"] {
        if let Some(value) = original.get(passthrough) {
            if !value.is_null() {
                out.insert(passthrough.to_string(), value.clone());
            }
        }
    }
    Some(JsonValue::Object(out))
}

/// Order the candidate targets by the persona's `prefer` list, so a
/// persona that lists `edit_rename_symbol` first surfaces that suggestion
/// when both a rename and a node edit fit.
fn prefers(config: &CompassConfig, target: StructuralTarget) -> bool {
    config.prefer.iter().any(|name| name == target.tool_name())
}

/// Core routing decision. Pure over `(tool_name, args, config)` — all
/// side effects (counters, reminders) happen in [`apply_decision`].
pub(crate) fn route(
    tool_name: &str,
    tool_args: &JsonValue,
    config: &CompassConfig,
) -> CompassDecision {
    if config.mode == CompassMode::Off {
        return CompassDecision::Passthrough;
    }
    let Some(edit) = classify_freeform_edit(tool_name, tool_args) else {
        return CompassDecision::Passthrough;
    };
    // Only steer edits on files the structural tools can actually parse.
    // A path-less call (rare) is steered too, since the agent still
    // benefits from the reminder.
    if let Some(path) = edit.path.as_deref() {
        if !parseable_extension(path) {
            return CompassDecision::Passthrough;
        }
    }

    // Pick the structural target this freeform edit maps to. A
    // single-token rename always points at `edit_rename_symbol` (the
    // failure mode the AST tools most dramatically fix); a whole-file
    // write points at node-level editing; everything else gets the
    // hash-guarded patch. A persona that explicitly prefers node-level
    // editing (its `edit_strategy.prefer` lists `edit_apply_node` ahead of
    // any patch tool) nudges a plain hunk edit toward `edit_apply_node`.
    let rename = single_token_rename(&edit.hunks);
    let target = if rename.is_some() {
        StructuralTarget::RenameSymbol
    } else if edit.whole_file || prefers(config, StructuralTarget::ApplyNode) {
        StructuralTarget::ApplyNode
    } else {
        StructuralTarget::SafeTextPatch
    };

    // Rewrite mode: substitute only when provably equivalent. The single
    // case we can prove without I/O is a raw text-replace -> the
    // hash-guarded `edit_safe_text_patch`. Anything else (rename, whole
    // file) is *not* byte-equivalent, so fall back to a suggestion.
    if config.mode == CompassMode::Rewrite {
        let already_safe_patch = tool_name
            .trim()
            .eq_ignore_ascii_case("edit_safe_text_patch");
        if !already_safe_patch && rename.is_none() && !edit.whole_file {
            if let Some(new_args) = rewrite_to_safe_text_patch(&edit, tool_args) {
                return CompassDecision::Rewrite {
                    target: StructuralTarget::SafeTextPatch,
                    tool_name: StructuralTarget::SafeTextPatch.tool_name().to_string(),
                    tool_args: new_args,
                };
            }
        }
        // Not provably equivalent — fall back (counted as fell_back in
        // apply_decision via the Suggest arm's fell_back flag).
    }

    let reminder_body = suggestion_body(tool_name, target, rename.as_ref());
    CompassDecision::Suggest {
        target,
        reminder_body,
    }
}

fn suggestion_body(
    tool_name: &str,
    target: StructuralTarget,
    rename: Option<&(String, String)>,
) -> String {
    match target {
        StructuralTarget::RenameSymbol => {
            let detail = rename
                .map(|(old, new)| format!(" Looks like a rename of `{old}` -> `{new}`; "))
                .unwrap_or_else(|| " ".to_string());
            format!(
                "[compass] `{tool_name}` is a freeform edit on a parseable file.{detail}\
                 prefer `edit_rename_symbol` so every caller and import is updated atomically \
                 instead of a single string match. Preview with `edit_dry_run` first."
            )
        }
        StructuralTarget::ApplyNode => format!(
            "[compass] `{tool_name}` rewrites a whole parseable file. Prefer node-level \
             `edit_apply_node` / `edit_insert_at_anchor` (target the changed declaration by AST \
             query) so untouched code keeps its exact bytes, or `edit_dry_run` to preview a plan."
        ),
        StructuralTarget::SafeTextPatch => format!(
            "[compass] `{tool_name}` is a freeform text edit on a parseable file. Prefer a \
             structural primitive (`edit_apply_node` for a node, `edit_rename_symbol` for a \
             symbol) — or at least `edit_safe_text_patch`, which hash-guards the pre-image and \
             writes atomically. Preview with `edit_dry_run`."
        ),
    }
}

/// Counter attributes shared by every compass decision. All keys live in
/// the `harn.compass.*` vocabulary so the audit gate accepts them.
fn counter_attrs(
    persona: &str,
    freeform_tool: &str,
    target: StructuralTarget,
) -> JsonMap<String, JsonValue> {
    let mut attrs = JsonMap::new();
    attrs.insert(
        "harn.compass.persona".to_string(),
        JsonValue::String(persona.to_string()),
    );
    attrs.insert(
        "harn.compass.tool".to_string(),
        JsonValue::String(freeform_tool.to_string()),
    );
    attrs.insert(
        "harn.compass.target".to_string(),
        JsonValue::String(target.tool_name().to_string()),
    );
    attrs
}

fn bump(name: &str, attrs: JsonMap<String, JsonValue>) {
    // Counters are best-effort observability; never let an emit failure
    // (e.g. no active backend) abort the tool call.
    let _ = emit_instrument(
        MetricInstrument::Counter,
        name.to_string(),
        JsonValue::from(1),
        attrs,
    );
}

/// Emit the counters (and surface the reminder body) for a decision. Returns
/// the reminder body to inject for `Suggest`, and `None` for the silent
/// `Rewrite` / `Passthrough` arms. `fell_back` is set by the caller when a
/// `Rewrite`-mode call could not be proven equivalent and degraded to a
/// suggestion.
pub(crate) fn apply_decision(
    decision: &CompassDecision,
    original_tool: &str,
    config: &CompassConfig,
    fell_back: bool,
) -> Option<String> {
    match decision {
        CompassDecision::Passthrough => None,
        CompassDecision::Rewrite { target, .. } => {
            bump(
                COUNTER_REWRITTEN,
                counter_attrs(&config.persona, original_tool, *target),
            );
            None
        }
        CompassDecision::Suggest {
            target,
            reminder_body,
        } => {
            let counter = if fell_back {
                COUNTER_FELL_BACK
            } else {
                COUNTER_SUGGESTED
            };
            bump(
                counter,
                counter_attrs(&config.persona, original_tool, *target),
            );
            Some(reminder_body.clone())
        }
    }
}

/// Convert an agent-loop options map into the JSON the config reader and
/// router consume.
pub(crate) fn options_to_json(options: &BTreeMap<String, crate::value::VmValue>) -> JsonValue {
    JsonValue::Object(
        options
            .iter()
            .map(|(key, value)| (key.clone(), crate::llm::helpers::vm_value_to_json(value)))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observability::vocabulary;
    use serde_json::json;

    fn cfg(mode: CompassMode) -> CompassConfig {
        CompassConfig {
            mode,
            persona: "fixer".to_string(),
            prefer: Vec::new(),
        }
    }

    #[test]
    fn off_mode_always_passes_through() {
        let args = json!({"path": "src/lib.rs", "old_text": "a", "new_text": "b"});
        assert_eq!(
            route("str_replace", &args, &cfg(CompassMode::Off)),
            CompassDecision::Passthrough
        );
    }

    #[test]
    fn structural_calls_are_never_rerouted() {
        let args = json!({"path": "src/lib.rs", "query": "(x)", "replacement": "{}"});
        assert_eq!(
            route("edit_apply_node", &args, &cfg(CompassMode::Suggest)),
            CompassDecision::Passthrough
        );
        assert_eq!(
            route("edit_rename_symbol", &args, &cfg(CompassMode::Rewrite)),
            CompassDecision::Passthrough
        );
    }

    #[test]
    fn non_parseable_file_passes_through() {
        let args = json!({"path": "README.md", "old_text": "a", "new_text": "b"});
        assert_eq!(
            route("str_replace", &args, &cfg(CompassMode::Suggest)),
            CompassDecision::Passthrough
        );
    }

    #[test]
    fn freeform_replace_on_source_file_suggests_in_suggest_mode() {
        let args =
            json!({"path": "src/lib.rs", "old_text": "let a = 1;", "new_text": "let a = 2;"});
        match route("str_replace", &args, &cfg(CompassMode::Suggest)) {
            CompassDecision::Suggest {
                target,
                reminder_body,
            } => {
                assert_eq!(target, StructuralTarget::SafeTextPatch);
                assert!(reminder_body.contains("edit_safe_text_patch"));
                assert!(reminder_body.contains("compass"));
            }
            other => panic!("expected Suggest, got {other:?}"),
        }
    }

    #[test]
    fn single_token_rename_suggests_rename_symbol() {
        let args = json!({"path": "src/lib.rs", "old_text": "Widget", "new_text": "Gadget"});
        match route("str_replace", &args, &cfg(CompassMode::Suggest)) {
            CompassDecision::Suggest {
                target,
                reminder_body,
            } => {
                assert_eq!(target, StructuralTarget::RenameSymbol);
                assert!(reminder_body.contains("edit_rename_symbol"));
                assert!(reminder_body.contains("Widget"));
                assert!(reminder_body.contains("Gadget"));
            }
            other => panic!("expected rename Suggest, got {other:?}"),
        }
    }

    #[test]
    fn rewrite_mode_substitutes_provably_equivalent_safe_patch() {
        let args = json!({"path": "src/lib.rs", "old_text": "let a = 1;", "new_text": "let a = 2;", "session_id": "s1"});
        match route("str_replace", &args, &cfg(CompassMode::Rewrite)) {
            CompassDecision::Rewrite {
                target,
                tool_name,
                tool_args,
            } => {
                assert_eq!(target, StructuralTarget::SafeTextPatch);
                assert_eq!(tool_name, "edit_safe_text_patch");
                assert_eq!(tool_args["path"], json!("src/lib.rs"));
                assert_eq!(tool_args["hunks"][0]["old_text"], json!("let a = 1;"));
                assert_eq!(tool_args["hunks"][0]["new_text"], json!("let a = 2;"));
                // session_id is preserved so the rewrite stays atomic.
                assert_eq!(tool_args["session_id"], json!("s1"));
            }
            other => panic!("expected Rewrite, got {other:?}"),
        }
    }

    #[test]
    fn rewrite_mode_falls_back_for_non_equivalent_rename() {
        // A rename is NOT byte-equivalent to a project-wide rename op, so
        // rewrite mode must degrade to a suggestion (the caller counts this
        // as fell_back).
        let args = json!({"path": "src/lib.rs", "old_text": "Widget", "new_text": "Gadget"});
        match route("str_replace", &args, &cfg(CompassMode::Rewrite)) {
            CompassDecision::Suggest { target, .. } => {
                assert_eq!(target, StructuralTarget::RenameSymbol);
            }
            other => panic!("expected fallback Suggest, got {other:?}"),
        }
    }

    #[test]
    fn whole_file_write_suggests_apply_node() {
        let args = json!({"path": "src/lib.rs", "content": "fn main() {}"});
        match route("write_file", &args, &cfg(CompassMode::Suggest)) {
            CompassDecision::Suggest {
                target,
                reminder_body,
            } => {
                assert_eq!(target, StructuralTarget::ApplyNode);
                assert!(reminder_body.contains("edit_apply_node"));
            }
            other => panic!("expected Suggest for whole-file write, got {other:?}"),
        }
        // Whole-file is not provably equivalent, so rewrite mode never
        // silently substitutes it.
        assert!(matches!(
            route("write_file", &args, &cfg(CompassMode::Rewrite)),
            CompassDecision::Suggest { .. }
        ));
    }

    #[test]
    fn config_defaults_to_suggest_when_compass_absent() {
        let config = CompassConfig::from_options(&json!({}));
        assert_eq!(config.mode, CompassMode::Suggest);
        assert_eq!(config.persona, "default");
    }

    #[test]
    fn config_disables_on_false_and_enabled_false() {
        assert_eq!(
            CompassConfig::from_options(&json!({"compass": false})).mode,
            CompassMode::Off
        );
        assert_eq!(
            CompassConfig::from_options(&json!({"compass": {"enabled": false}})).mode,
            CompassMode::Off
        );
        assert_eq!(
            CompassConfig::from_options(&json!({"compass": {"mode": "off"}})).mode,
            CompassMode::Off
        );
    }

    #[test]
    fn config_reads_mode_persona_and_prefer() {
        let config = CompassConfig::from_options(&json!({
            "persona": "fixer",
            "compass": {"mode": "rewrite", "prefer": ["edit_rename_symbol"]},
        }));
        assert_eq!(config.mode, CompassMode::Rewrite);
        assert_eq!(config.persona, "fixer");
        assert_eq!(config.prefer, vec!["edit_rename_symbol".to_string()]);
    }

    #[test]
    fn config_falls_back_to_edit_strategy_prefer_signal() {
        // The fixer persona declares edit_strategy.prefer; the compass
        // consumes it when no compass.prefer override is set (#2612).
        let config = CompassConfig::from_options(&json!({
            "edit_strategy": {"prefer": ["edit_apply_node", "edit_insert_at_anchor"]},
        }));
        assert_eq!(
            config.prefer,
            vec![
                "edit_apply_node".to_string(),
                "edit_insert_at_anchor".to_string()
            ]
        );
    }

    #[test]
    fn apply_decision_counts_suggested_and_returns_body() {
        let decision = CompassDecision::Suggest {
            target: StructuralTarget::SafeTextPatch,
            reminder_body: "body".to_string(),
        };
        let body = apply_decision(&decision, "str_replace", &cfg(CompassMode::Suggest), false);
        assert_eq!(body.as_deref(), Some("body"));
    }

    #[test]
    fn apply_decision_rewrite_is_silent() {
        let decision = CompassDecision::Rewrite {
            target: StructuralTarget::SafeTextPatch,
            tool_name: "edit_safe_text_patch".to_string(),
            tool_args: json!({}),
        };
        let body = apply_decision(&decision, "str_replace", &cfg(CompassMode::Rewrite), false);
        assert_eq!(body, None);
    }

    #[test]
    fn apply_decision_increments_compass_counter() {
        use crate::stdlib::observability;
        // The `test` backend echoes the full event (name + attrs) without
        // printing, and a reset gives us a clean buffer to assert against.
        observability::reset_observability_state();
        observability::install_default_backend("test").expect("install test backend");

        let decision = CompassDecision::Suggest {
            target: StructuralTarget::SafeTextPatch,
            reminder_body: "body".to_string(),
        };
        let _ = apply_decision(&decision, "str_replace", &cfg(CompassMode::Suggest), false);

        let emissions = observability::captured_emissions();
        let blob = serde_json::to_string(&emissions).unwrap();
        assert!(
            blob.contains("harn.compass.suggested"),
            "expected a harn.compass.suggested counter, got: {blob}"
        );
        assert!(
            blob.contains("str_replace"),
            "expected the freeform tool tag"
        );
        observability::reset_observability_state();
    }

    #[test]
    fn apply_decision_fell_back_uses_fell_back_counter() {
        use crate::stdlib::observability;
        observability::reset_observability_state();
        observability::install_default_backend("test").expect("install test backend");

        let decision = CompassDecision::Suggest {
            target: StructuralTarget::RenameSymbol,
            reminder_body: "body".to_string(),
        };
        // fell_back=true models a rewrite-mode call that could not be
        // proven equivalent and degraded to a suggestion.
        let _ = apply_decision(&decision, "str_replace", &cfg(CompassMode::Rewrite), true);

        let blob = serde_json::to_string(&observability::captured_emissions()).unwrap();
        assert!(
            blob.contains("harn.compass.fell_back"),
            "expected harn.compass.fell_back counter, got: {blob}"
        );
        observability::reset_observability_state();
    }

    #[test]
    fn counter_attrs_use_compass_vocabulary() {
        let attrs = counter_attrs("fixer", "str_replace", StructuralTarget::SafeTextPatch);
        for key in attrs.keys() {
            assert!(
                !vocabulary::is_violation(key),
                "attr `{key}` must be in the compass vocabulary"
            );
        }
    }
}
