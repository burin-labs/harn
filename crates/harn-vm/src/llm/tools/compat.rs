//! Provider/model compatibility cleanup for tool-call envelopes.
//!
//! Some OpenAI-compatible hosts leak model-native wrappers into otherwise
//! structured tool calls. GPT-OSS/Harmony is the common case: a call can arrive
//! as a generic `tool`/`tool.call`/`tool.exec` function whose arguments contain
//! the real `{ name, args }`, or the tool name can carry a channel suffix such
//! as `run<|channel|>commentary`. Some providers also surface marker-only
//! wrapper names such as `<|constrain|>json`; normalize those shapes before
//! policy and dispatch see them.

const HARMONY_CHANNEL_MARKERS: &[&str] =
    &["<|channel|>", "<|message|>", "<|recipient|>", "<|end|>"];

/// Namespace prefixes that cheap/OpenAI-compatible hosts (notably gpt-oss-120b)
/// prepend to otherwise-bare tool names, e.g. `tool.look`, `functions.search`.
/// These are stripped only for names that are NOT generic wrappers, so that
/// `tool.call`/`tool.exec`/`function.call` still route through
/// [`unwrap_generic_tool_arguments`] to recover their inner `{ name, args }`.
const TOOL_NAMESPACE_PREFIXES: &[&str] = &["tool.", "tools.", "functions.", "function."];

/// Strip provider protocol tokens that were appended to a function name.
pub(crate) fn normalize_tool_name(name: &str) -> String {
    let trimmed = name.trim();
    for marker in HARMONY_CHANNEL_MARKERS {
        if let Some(pos) = trimmed.find(marker) {
            let head = trimmed[..pos].trim();
            if !head.is_empty() {
                return head.to_string();
            }
        }
    }
    trimmed.to_string()
}

/// Strip a leading `tool.`/`tools.`/`functions.`/`function.` namespace prefix
/// from a tool name, e.g. `tool.look` -> `look`. Returns `None` when no prefix
/// applies or when stripping would yield an empty name. Callers MUST guard
/// against generic wrapper names (see [`is_generic_wrapper_name`]) before
/// applying this, so that `tool.call`/`tool.exec`/`function.call` keep their
/// wrapper-unwrapping path instead of collapsing to `call`/`exec`.
fn strip_tool_namespace_prefix(name: &str) -> Option<String> {
    let trimmed = name.trim();
    for prefix in TOOL_NAMESPACE_PREFIXES {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let bare = rest.trim();
            if !bare.is_empty() {
                return Some(bare.to_string());
            }
        }
    }
    None
}

/// Recover a bare tool name from a namespace-prefixed one (`tool.look` ->
/// `look`), but only when the name is NOT a generic wrapper. The guard keeps
/// `tool.call`/`tool.exec`/`function.call` intact so they are never collapsed
/// to `call`/`exec` — those are unwrapped via [`unwrap_generic_tool_arguments`]
/// before this is reached. Used both for the top-level name and for the inner
/// name recovered from a generic wrapper.
fn recover_namespaced_name(name: String) -> String {
    if is_generic_wrapper_name(&name) {
        return name;
    }
    strip_tool_namespace_prefix(&name).unwrap_or(name)
}

/// Normalize a parsed tool-call `(name, arguments)` pair into the dispatchable
/// Harn shape.
pub(crate) fn normalize_tool_call_shape(
    name: &str,
    arguments: serde_json::Value,
) -> (String, serde_json::Value) {
    let normalized_name = normalize_tool_name(name);
    let is_marker_wrapper = is_harmony_marker_wrapper_name(&normalized_name);
    if !is_generic_wrapper_name(&normalized_name) && !is_marker_wrapper {
        let recovered = recover_namespaced_name(normalized_name);
        return resolve_semantic_alias(recovered, arguments);
    }

    if let Some((inner_name, inner_arguments)) = unwrap_generic_tool_arguments(&arguments) {
        let recovered = recover_namespaced_name(normalize_tool_name(&inner_name));
        return resolve_semantic_alias(recovered, inner_arguments);
    }

    if is_marker_wrapper || is_generic_wrapper_name(&normalized_name) {
        if let Some(inferred_name) = infer_tool_name_from_arguments(&arguments) {
            return (inferred_name, arguments);
        }
    }

    // Last resort for marker-wrapper names nothing above could unwrap or
    // infer (e.g. `<|constrain|>json` with an unrecognized argument shape):
    // strip the `<|...|>` provider template tokens so the call fails as a
    // clean unknown-name denial — or resolves, when the remainder is a real
    // tool/alias — instead of leaking raw protocol tokens into the gate,
    // denial feedback, and telemetry.
    if is_marker_wrapper {
        let sanitized = strip_marker_tokens(&normalized_name);
        if !sanitized.is_empty() {
            return resolve_semantic_alias(sanitized, arguments);
        }
    }

    (normalized_name, arguments)
}

/// Remove every `<|...|>` provider template token span from a tool name.
/// An unterminated `<|` swallows the rest of the name (it is all marker
/// residue). Returns the trimmed remainder, which may be empty.
fn strip_marker_tokens(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(start) = rest.find("<|") {
        out.push_str(&rest[..start]);
        match rest[start..].find("|>") {
            Some(end) => rest = &rest[start + end + 2..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// Canonicalize SEMANTIC tool aliases that cheap models emit even though the
/// name was never advertised, so the gate, dispatch, and telemetry all see a
/// real Harn tool name. Grounded in live fw-gpt-oss-120b denial transcripts:
/// Codex `repo_browser.*` vocab, `container.exec`, and edit-action verbs called
/// as top-level tools. The targets here (`look`, `search`, `run`, `edit`) are
/// the canonical Harn coding tools the executor recognizes, so the rewrite is
/// safe without a per-turn allowed-tool set.
///
/// NOT mapped, deliberately: `write_file` / `delete_file` / `patch_file` →
/// `edit`. Those are semantically lossy (`edit` is a structured-patch tool with
/// no raw-write/whole-file-create semantics); silently rewriting them would
/// rename a call then fail arg validation. They are left UNmapped so the
/// actionable denial feedback (naming the available tools) can guide the model.
fn resolve_semantic_alias(
    name: String,
    arguments: serde_json::Value,
) -> (String, serde_json::Value) {
    // Canonical `run` calls can still arrive with Codex/OpenAI-style argv
    // payloads (`{"command":["bash","lc","..."]}`) from native providers.
    // Normalize those arguments even when no alias rewrite is needed.
    if name == "run" {
        return (name, remap_command_args(arguments));
    }

    // 1. Browser-namespace aliases (Codex / Aider-style file-explorer vocab).
    //    Strip a known `*_browser.` prefix, then map the verb to look/search.
    if let Some((canonical, mapped_args)) = resolve_browser_alias(&name, &arguments) {
        return (canonical, mapped_args);
    }

    // 2. Shell/exec aliases → run. Remap a command-shaped arg key onto
    //    `command` so the rename doesn't then fail run's arg validation.
    if is_shell_alias(&name) {
        return ("run".to_string(), remap_command_args(arguments));
    }

    // 3. Edit-action verbs called as top-level tools → edit({action: <verb>}).
    if is_edit_action_verb(&name) {
        return ("edit".to_string(), remap_edit_action_args(&name, arguments));
    }

    (name, arguments)
}

/// Known file-explorer namespace prefixes cheap models borrow from other
/// harnesses (Codex `repo_browser.*`, plus the obvious siblings).
const BROWSER_NAMESPACE_PREFIXES: &[&str] = &[
    "repo_browser.",
    "repository_browser.",
    "workspace_browser.",
    "file_browser.",
];

fn resolve_browser_alias(
    name: &str,
    arguments: &serde_json::Value,
) -> Option<(String, serde_json::Value)> {
    let verb = BROWSER_NAMESPACE_PREFIXES
        .iter()
        .find_map(|prefix| name.strip_prefix(prefix))?
        .trim();
    if verb.is_empty() {
        return None;
    }
    let canonical = match verb {
        "open_file" | "view_file" | "cat_file" | "read_file" | "print_tree" | "list_tree"
        | "list_dir" | "list_directory" | "ls" => "look",
        "search" | "find" | "grep" => "search",
        // `*.bundle` is left for downstream recovery: `bundle` is not a
        // universally registered tool, so rewriting to it could itself trip the
        // ceiling. Keep the prefix-stripped bare name so the denial feedback can
        // guide the model rather than silently producing another unknown tool.
        _ => return Some((verb.to_string(), arguments.clone())),
    };
    Some((canonical.to_string(), arguments.clone()))
}

fn is_shell_alias(name: &str) -> bool {
    matches!(
        name,
        "container.exec" | "container_exec" | "exec" | "sh" | "shell" | "bash"
    )
}

/// Move a command-shaped argument (`script` / `cmd`) onto `command`/`argv` so the
/// remapped `run` call passes arg validation. Leaves an already-`command`-shaped
/// or unrecognized object untouched.
fn remap_command_args(arguments: serde_json::Value) -> serde_json::Value {
    let serde_json::Value::Object(mut map) = arguments else {
        return arguments;
    };
    if let Some(value) = map.remove("command") {
        insert_normalized_run_command_arg(&mut map, value);
        return serde_json::Value::Object(map);
    }
    for key in ["script", "cmd"] {
        if let Some(value) = map.remove(key) {
            insert_normalized_run_command_arg(&mut map, value);
            break;
        }
    }
    serde_json::Value::Object(map)
}

enum NormalizedRunCommandArg {
    Command(serde_json::Value),
    Argv(serde_json::Value),
}

fn insert_normalized_run_command_arg(
    map: &mut serde_json::Map<String, serde_json::Value>,
    value: serde_json::Value,
) {
    match normalize_command_value(value) {
        NormalizedRunCommandArg::Command(value) => {
            map.insert("command".to_string(), value);
        }
        NormalizedRunCommandArg::Argv(value) => {
            map.insert("argv".to_string(), value);
        }
    }
}

fn normalize_command_value(value: serde_json::Value) -> NormalizedRunCommandArg {
    let serde_json::Value::Array(items) = value else {
        return NormalizedRunCommandArg::Command(value);
    };
    let Some(argv) = items
        .iter()
        .map(serde_json::Value::as_str)
        .map(|value| value.map(str::to_string))
        .collect::<Option<Vec<_>>>()
    else {
        return NormalizedRunCommandArg::Command(serde_json::Value::Array(items));
    };
    if argv.len() == 3 && is_shell_argv_prefix(argv[0].as_str(), argv[1].as_str()) {
        return NormalizedRunCommandArg::Command(serde_json::Value::String(argv[2].clone()));
    }
    NormalizedRunCommandArg::Argv(serde_json::Value::Array(
        argv.into_iter()
            .map(serde_json::Value::String)
            .collect::<Vec<_>>(),
    ))
}

fn is_shell_argv_prefix(binary: &str, flag: &str) -> bool {
    matches!(binary, "bash" | "sh" | "/bin/bash" | "/bin/sh")
        && matches!(flag, "-lc" | "-c" | "lc" | "c")
}

/// Edit-action verbs that exist ONLY as `edit({ action: <verb> })` enum values,
/// never as standalone advertised tool names — so folding a top-level call of
/// one into `edit` cannot shadow a real tool.
///
/// `replace_symbol` / `remove_symbol` are deliberately EXCLUDED: `replace_symbol`
/// is a hard-kept standalone tool name in Harn's default agent tool surface
/// (`__default_tool_surface_hard_keep`), so a top-level `replace_symbol` call is
/// a LEGITIMATE call the gate allows — rewriting it to `edit` would shadow a
/// real tool and lose the symbol-level semantics. `remove_symbol` is excluded
/// for the same symbol-tool symmetry. Both fall through unmapped.
const EDIT_ACTION_VERBS: &[&str] = &[
    "create",
    "replace_range",
    "replace_body",
    "insert_after",
    "insert_function",
    "delete_range",
    "exact_patch",
    "add_import",
];

fn is_edit_action_verb(name: &str) -> bool {
    EDIT_ACTION_VERBS.contains(&name)
}

/// Fold an edit-action-verb-as-tool into `edit({ action: <verb>, ...rest })`.
/// The verb's own arguments ride through unchanged under the `edit` envelope;
/// only the `action` key is injected (an existing `action` is preserved).
fn remap_edit_action_args(verb: &str, arguments: serde_json::Value) -> serde_json::Value {
    let mut map = match arguments {
        serde_json::Value::Object(map) => map,
        serde_json::Value::Null => serde_json::Map::new(),
        other => {
            // Non-object args can't carry an `action` key; wrap defensively so
            // the rename still produces a valid edit envelope shape.
            let mut map = serde_json::Map::new();
            map.insert("value".to_string(), other);
            map
        }
    };
    map.entry("action".to_string())
        .or_insert_with(|| serde_json::Value::String(verb.to_string()));
    serde_json::Value::Object(map)
}

pub(crate) fn is_generic_wrapper_name(name: &str) -> bool {
    matches!(
        name,
        "tool" | "tool.call" | "tool.exec" | "tool_call" | "function" | "function.call" | "call"
    )
}

fn is_harmony_marker_wrapper_name(name: &str) -> bool {
    let trimmed = name.trim();
    trimmed.starts_with("<|") && trimmed.contains("|>")
}

fn infer_tool_name_from_arguments(arguments: &serde_json::Value) -> Option<String> {
    let object = arguments.as_object()?;
    let intent = object
        .get("intent")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .map(str::to_ascii_lowercase);
    match intent.as_deref() {
        Some("read" | "open" | "look" | "view" | "list" | "ls") => {
            return Some("look".to_string());
        }
        Some("search" | "grep" | "find") => return Some("search".to_string()),
        _ => {}
    }

    let has_command_shape = object
        .get("command")
        .or_else(|| object.get("commands"))
        .or_else(|| object.get("cmd"))
        .is_some();
    if has_command_shape {
        return Some("run".to_string());
    }

    None
}

/// Coerce string-typed argument values onto the shape the tool's registered
/// schema expects, at the dispatch chokepoint every call passes through.
///
/// Grounded in eval-corpus runtime failures: value models deliver
/// `overwrite: "True"` (Python spelling) where the schema says bool, and a
/// JSON-encoded STRING (`ops: "[{...}]"`) where the schema says list — both
/// then die in VM typed-parameter validation. Coercion is applied ONLY on an
/// unambiguous schema expectation:
///
/// * bool: the param type admits booleans (and at most `null`) — a
///   `string | bool` union stays untouched — and the value is one of the
///   exact spellings `true`/`True`/`TRUE`/`false`/`False`/`FALSE`.
/// * list: the param type admits arrays (and at most `null`) and the string
///   parses cleanly as a JSON array. A string that fails to parse rides
///   through unchanged so the precise type error still surfaces.
pub(crate) fn coerce_args_to_schema(
    name: &str,
    arguments: serde_json::Value,
    tools_val: Option<&crate::value::VmValue>,
) -> serde_json::Value {
    let serde_json::Value::Object(mut map) = arguments else {
        return arguments;
    };
    let Some(schema) = super::collect_tool_schemas(tools_val, None)
        .into_iter()
        .find(|schema| schema.name == name)
    else {
        return serde_json::Value::Object(map);
    };
    for param in &schema.params {
        let Some(value) = map.get_mut(&param.name) else {
            continue;
        };
        let serde_json::Value::String(raw) = &*value else {
            continue;
        };
        if type_expr_wants_bool(&param.ty) {
            match raw.trim() {
                "true" | "True" | "TRUE" => *value = serde_json::Value::Bool(true),
                "false" | "False" | "FALSE" => *value = serde_json::Value::Bool(false),
                _ => {}
            }
        } else if type_expr_wants_list(&param.ty) {
            let trimmed = raw.trim();
            if trimmed.starts_with('[') {
                if let Ok(parsed @ serde_json::Value::Array(_)) =
                    serde_json::from_str::<serde_json::Value>(trimmed)
                {
                    *value = parsed;
                }
            }
        }
    }
    serde_json::Value::Object(map)
}

/// True when the schema type UNAMBIGUOUSLY expects a boolean: a bare bool
/// primitive/literal, or a union whose members are all bool (plus at most
/// `null`). A union that also admits strings is ambiguous — never coerce.
fn type_expr_wants_bool(ty: &super::type_expr::TypeExpr) -> bool {
    use super::type_expr::TypeExpr;
    match ty {
        // Pipeline VM-dict schemas keep the raw Harn spelling (`bool`);
        // JSON-schema-derived ones use `boolean`. Accept both.
        TypeExpr::Primitive(name) => name == "boolean" || name == "bool",
        TypeExpr::Literal(value) => value.is_boolean(),
        TypeExpr::Union(items) => {
            items.iter().any(type_expr_wants_bool)
                && items.iter().all(|item| {
                    type_expr_wants_bool(item)
                        || matches!(item, TypeExpr::Primitive(name) if name == "null")
                })
        }
        _ => false,
    }
}

/// True when the schema type UNAMBIGUOUSLY expects a list: an array type, or
/// a union whose members are all arrays (plus at most `null`).
fn type_expr_wants_list(ty: &super::type_expr::TypeExpr) -> bool {
    use super::type_expr::TypeExpr;
    match ty {
        TypeExpr::Array(_) => true,
        // Pipeline VM-dict schemas keep the raw Harn spelling (`list`);
        // JSON-schema-derived ones use `array`. Accept both.
        TypeExpr::Primitive(name) => name == "array" || name == "list",
        TypeExpr::Union(items) => {
            items.iter().any(type_expr_wants_list)
                && items.iter().all(|item| {
                    type_expr_wants_list(item)
                        || matches!(item, TypeExpr::Primitive(name) if name == "null")
                })
        }
        _ => false,
    }
}

fn unwrap_generic_tool_arguments(
    arguments: &serde_json::Value,
) -> Option<(String, serde_json::Value)> {
    let object = arguments.as_object()?;
    let inner_name = object
        .get("name")
        .or_else(|| object.get("tool"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_string();
    let inner_arguments = match object
        .get("args")
        .or_else(|| object.get("arguments"))
        .or_else(|| object.get("parameters"))
    {
        Some(value @ serde_json::Value::Object(_)) => value.clone(),
        Some(serde_json::Value::String(raw)) => match serde_json::from_str(raw) {
            Ok(value @ serde_json::Value::Object(_)) => value,
            _ => return None,
        },
        // No args envelope at all: cheap models (observed live: fw-gpt-oss-120b
        // emitting `tool_call({"name":"look","intent":"read","path":"..."})`)
        // FLATTEN the real arguments beside the inner name. Returning empty
        // args here silently dropped them and the recovered call then died on
        // required-parameter validation. Carry every sibling key through as
        // the inner arguments instead.
        Some(serde_json::Value::Null) | None => {
            let mut flattened = object.clone();
            flattened.remove("name");
            flattened.remove("tool");
            flattened.remove("args");
            flattened.remove("arguments");
            flattened.remove("parameters");
            serde_json::Value::Object(flattened)
        }
        Some(_) => return None,
    };

    Some((inner_name, inner_arguments))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_harmony_channel_suffix_from_tool_name() {
        assert_eq!(normalize_tool_name("run<|channel|>commentary"), "run");
    }

    #[test]
    fn unwraps_generic_tool_envelope() {
        let (name, arguments) = normalize_tool_call_shape(
            "tool.call",
            serde_json::json!({
                "name": "look",
                "args": {"intent": "read", "file": "src/lib.rs"}
            }),
        );

        assert_eq!(name, "look");
        assert_eq!(arguments["intent"], "read");
        assert_eq!(arguments["file"], "src/lib.rs");
    }

    #[test]
    fn unwraps_generic_wrapper_with_flattened_sibling_args() {
        // Real fw-gpt-oss-120b corpus shape: the wrapper call carries the
        // inner name AND the real arguments flattened beside it — no `args`
        // envelope. The siblings must ride through as the inner arguments.
        let (name, arguments) = normalize_tool_call_shape(
            "tool_call",
            serde_json::json!({
                "name": "look",
                "intent": "read",
                "path": "src/document.zig",
                "reason": "view document structures"
            }),
        );

        assert_eq!(name, "look");
        assert_eq!(arguments["intent"], "read");
        assert_eq!(arguments["path"], "src/document.zig");
        assert!(arguments.get("name").is_none());
    }

    #[test]
    fn infers_run_from_harmony_marker_wrapper_command_shape() {
        let arguments = serde_json::json!({"command": "cargo test"});
        let (name, normalized_arguments) =
            normalize_tool_call_shape("<|constrain|>json", arguments.clone());

        assert_eq!(name, "run");
        assert_eq!(normalized_arguments, arguments);
    }

    #[test]
    fn infers_look_from_harmony_marker_wrapper_read_intent() {
        let arguments = serde_json::json!({"intent": "read", "file": "src/lib.rs"});
        let (name, normalized_arguments) =
            normalize_tool_call_shape("<|constrain|>json", arguments.clone());

        assert_eq!(name, "look");
        assert_eq!(normalized_arguments, arguments);
    }

    #[test]
    fn infers_search_from_tool_call_wrapper_search_intent() {
        let arguments = serde_json::json!({"intent": "search", "path": "src", "query": "fn parse"});
        let (name, normalized_arguments) =
            normalize_tool_call_shape("tool_call", arguments.clone());

        assert_eq!(name, "search");
        assert_eq!(normalized_arguments, arguments);
    }

    #[test]
    fn sanitizes_unrecognized_harmony_marker_wrapper_name() {
        // Nothing to unwrap or infer: the `<|...|>` provider tokens are
        // stripped so the call fails as a clean unknown-name denial instead of
        // leaking raw protocol tokens into the gate and telemetry.
        let arguments = serde_json::json!({"intent": "summarize", "file": "src/lib.rs"});
        let (name, normalized_arguments) =
            normalize_tool_call_shape("<|constrain|>json", arguments.clone());

        assert_eq!(name, "json");
        assert_eq!(normalized_arguments, arguments);
    }

    #[test]
    fn marker_wrapper_remainder_resolves_via_alias_table() {
        // The sanitized remainder is a shell synonym: resolve it like any
        // other alias instead of denying on the marker-wrapped name.
        let (name, mapped) = normalize_tool_call_shape(
            "<|constrain|>bash",
            serde_json::json!({"script": "cargo test"}),
        );
        assert_eq!(name, "run");
        assert_eq!(mapped["command"], "cargo test");
    }

    #[test]
    fn all_marker_tokens_name_keeps_wrapper_shape() {
        // A name that is NOTHING BUT marker tokens has no remainder to
        // resolve; keep the wrapper name so the denial names what arrived.
        let arguments = serde_json::json!({"payload": 1});
        let (name, _) = normalize_tool_call_shape("<|constrain|>", arguments);
        assert_eq!(name, "<|constrain|>");
    }

    #[test]
    fn preserves_non_wrapper_tool_call() {
        let arguments = serde_json::json!({"command": "cargo test"});
        let (name, normalized_arguments) = normalize_tool_call_shape("run", arguments.clone());

        assert_eq!(name, "run");
        assert_eq!(normalized_arguments, arguments);
    }

    #[test]
    fn strips_tool_namespace_prefix_from_native_name() {
        let arguments = serde_json::json!({"intent": "read", "file": "src/lib.rs"});
        let (name, normalized_arguments) =
            normalize_tool_call_shape("tool.look", arguments.clone());

        assert_eq!(name, "look");
        assert_eq!(normalized_arguments, arguments);
    }

    #[test]
    fn strips_functions_namespace_prefix_from_native_name() {
        let arguments = serde_json::json!({"query": "needle"});
        let (name, normalized_arguments) =
            normalize_tool_call_shape("functions.search", arguments.clone());

        assert_eq!(name, "search");
        assert_eq!(normalized_arguments, arguments);
    }

    #[test]
    fn strips_tools_and_function_namespace_prefixes() {
        let (name, _) = normalize_tool_call_shape("tools.run", serde_json::json!({}));
        assert_eq!(name, "run");
        // `function.edit` is NOT a generic wrapper (only `function.call` is), so
        // it takes the prefix-strip path.
        let (name, _) = normalize_tool_call_shape("function.edit", serde_json::json!({}));
        assert_eq!(name, "edit");
    }

    #[test]
    fn generic_tool_exec_wrapper_unwraps_inner_name_not_prefix_stripped() {
        // Collision guard: `tool.exec` shares the `tool.` prefix but is a
        // generic wrapper. It must unwrap its inner `{name, args}`, never be
        // naively stripped to `exec`.
        let (name, arguments) = normalize_tool_call_shape(
            "tool.exec",
            serde_json::json!({
                "name": "run",
                "args": {"command": "cargo test"}
            }),
        );

        assert_eq!(name, "run");
        assert_eq!(arguments["command"], "cargo test");
    }

    #[test]
    fn generic_function_call_wrapper_is_not_prefix_stripped() {
        // `function.call` shares the `function.` prefix but is a generic
        // wrapper; it must unwrap rather than collapse to `call`.
        let (name, arguments) = normalize_tool_call_shape(
            "function.call",
            serde_json::json!({
                "name": "search",
                "args": {"query": "needle"}
            }),
        );

        assert_eq!(name, "search");
        assert_eq!(arguments["query"], "needle");
    }

    #[test]
    fn strips_namespace_prefix_after_harmony_suffix() {
        // A name can carry both a namespace prefix and a harmony suffix; the
        // suffix is removed first, then the prefix.
        let (name, _) =
            normalize_tool_call_shape("tool.run<|channel|>commentary", serde_json::json!({}));
        assert_eq!(name, "run");
    }

    #[test]
    fn leaves_already_bare_name_unchanged() {
        let arguments = serde_json::json!({"intent": "read"});
        let (name, normalized_arguments) = normalize_tool_call_shape("look", arguments.clone());

        assert_eq!(name, "look");
        assert_eq!(normalized_arguments, arguments);
    }

    // ── F2: semantic alias resolution (cheap-model dialect) ──────────────

    #[test]
    fn aliases_repo_browser_open_file_to_look() {
        let args = serde_json::json!({"path": "src/lib.rs"});
        let (name, mapped) = normalize_tool_call_shape("repo_browser.open_file", args.clone());
        assert_eq!(name, "look");
        assert_eq!(mapped, args);
    }

    #[test]
    fn aliases_repo_browser_list_dir_to_look() {
        let (name, _) = normalize_tool_call_shape("repo_browser.list_dir", serde_json::json!({}));
        assert_eq!(name, "look");
        // Sibling prefixes resolve the same way.
        let (name, _) = normalize_tool_call_shape("file_browser.print_tree", serde_json::json!({}));
        assert_eq!(name, "look");
    }

    #[test]
    fn aliases_repo_browser_search_to_search() {
        let args = serde_json::json!({"query": "needle"});
        let (name, mapped) = normalize_tool_call_shape("repository_browser.grep", args.clone());
        assert_eq!(name, "search");
        assert_eq!(mapped, args);
    }

    #[test]
    fn browser_bundle_is_left_for_downstream_recovery() {
        // `bundle` is not universally registered; rewriting to it could itself
        // trip the ceiling. The prefix is stripped but the verb is left bare so
        // the actionable denial feedback can guide the model.
        let (name, _) =
            normalize_tool_call_shape("workspace_browser.bundle", serde_json::json!({}));
        assert_eq!(name, "bundle");
    }

    #[test]
    fn aliases_container_exec_to_run_remapping_command_arg() {
        let (name, mapped) =
            normalize_tool_call_shape("container.exec", serde_json::json!({"cmd": "cargo test"}));
        assert_eq!(name, "run");
        // `cmd` is remapped onto `command` so run's arg validation passes.
        assert_eq!(mapped["command"], "cargo test");
        assert!(mapped.get("cmd").is_none());
    }

    #[test]
    fn aliases_container_exec_argv_to_run_command_string() {
        let (name, mapped) = normalize_tool_call_shape(
            "container.exec",
            serde_json::json!({"cmd": ["bash", "-lc", "ls -R"], "timeout_ms": 1000}),
        );
        assert_eq!(name, "run");
        assert_eq!(mapped["command"], "ls -R");
        assert_eq!(mapped["timeout_ms"], 1000);
        assert!(mapped.get("cmd").is_none());
    }

    #[test]
    fn aliases_container_exec_malformed_shell_argv_to_script_string() {
        let (name, mapped) = normalize_tool_call_shape(
            "container.exec",
            serde_json::json!({"command": ["bash", "lc", "ls -R"]}),
        );
        assert_eq!(name, "run");
        assert_eq!(mapped["command"], "ls -R");
    }

    #[test]
    fn aliases_container_exec_general_argv_to_structured_argv() {
        let (name, mapped) = normalize_tool_call_shape(
            "container.exec",
            serde_json::json!({"cmd": ["git", "commit", "-m", "fix tool calls"]}),
        );
        assert_eq!(name, "run");
        assert_eq!(
            mapped["argv"],
            serde_json::json!(["git", "commit", "-m", "fix tool calls"])
        );
        assert!(mapped.get("command").is_none());
    }

    #[test]
    fn normalizes_canonical_run_argv_command_string() {
        let (name, mapped) = normalize_tool_call_shape(
            "run",
            serde_json::json!({"command": ["bash", "lc", "ls -R"]}),
        );
        assert_eq!(name, "run");
        assert_eq!(mapped["command"], "ls -R");
    }

    #[test]
    fn normalizes_canonical_run_general_argv_to_structured_argv() {
        let (name, mapped) = normalize_tool_call_shape(
            "run",
            serde_json::json!({"command": ["python3", "-c", "print('a b')"]}),
        );
        assert_eq!(name, "run");
        assert_eq!(
            mapped["argv"],
            serde_json::json!(["python3", "-c", "print('a b')"])
        );
        assert!(mapped.get("command").is_none());
    }

    #[test]
    fn normalizes_canonical_run_cmd_alias() {
        let (name, mapped) = normalize_tool_call_shape(
            "run",
            serde_json::json!({"cmd": ["bash", "-lc", "swift test"], "timeout_ms": 1000}),
        );
        assert_eq!(name, "run");
        assert_eq!(mapped["command"], "swift test");
        assert_eq!(mapped["timeout_ms"], 1000);
        assert!(mapped.get("cmd").is_none());
    }

    #[test]
    fn normalizes_namespaced_run_general_argv_to_structured_argv() {
        let (name, mapped) = normalize_tool_call_shape(
            "functions.run",
            serde_json::json!({"command": ["git", "status", "--short"]}),
        );
        assert_eq!(name, "run");
        assert_eq!(
            mapped["argv"],
            serde_json::json!(["git", "status", "--short"])
        );
        assert!(mapped.get("command").is_none());
    }

    #[test]
    fn aliases_shell_synonyms_to_run() {
        for alias in ["exec", "sh", "shell", "bash", "container_exec"] {
            let (name, mapped) =
                normalize_tool_call_shape(alias, serde_json::json!({"script": "ls"}));
            assert_eq!(name, "run", "alias {alias} should map to run");
            assert_eq!(mapped["command"], "ls", "alias {alias} should remap script");
        }
    }

    #[test]
    fn run_alias_leaves_existing_command_arg_untouched() {
        let (name, mapped) = normalize_tool_call_shape(
            "exec",
            serde_json::json!({"command": "ls", "cmd": "ignored"}),
        );
        assert_eq!(name, "run");
        assert_eq!(mapped["command"], "ls");
    }

    #[test]
    fn aliases_edit_action_verb_to_edit_with_action_arg() {
        let (name, mapped) = normalize_tool_call_shape(
            "replace_range",
            serde_json::json!({"path": "a.go", "range_start": 3, "range_end": 5}),
        );
        assert_eq!(name, "edit");
        assert_eq!(mapped["action"], "replace_range");
        assert_eq!(mapped["path"], "a.go");
        assert_eq!(mapped["range_start"], 3);
    }

    #[test]
    fn edit_action_verb_preserves_explicit_action() {
        // A pre-existing `action` is not clobbered by the verb.
        let (name, mapped) = normalize_tool_call_shape(
            "replace_body",
            serde_json::json!({"action": "replace_range", "path": "a.go"}),
        );
        assert_eq!(name, "edit");
        assert_eq!(mapped["action"], "replace_range");
    }

    #[test]
    fn all_edit_action_verbs_map_to_edit() {
        for verb in [
            "create",
            "replace_range",
            "replace_body",
            "insert_after",
            "insert_function",
            "delete_range",
            "exact_patch",
            "add_import",
        ] {
            let (name, mapped) = normalize_tool_call_shape(verb, serde_json::json!({}));
            assert_eq!(name, "edit", "verb {verb} should map to edit");
            assert_eq!(mapped["action"], verb);
        }
    }

    #[test]
    fn aliases_create_verb_to_edit_create_action() {
        let (name, mapped) = normalize_tool_call_shape(
            "create",
            serde_json::json!({"path": "src/status.go", "content": "package cli\n"}),
        );
        assert_eq!(name, "edit");
        assert_eq!(mapped["action"], "create");
        assert_eq!(mapped["path"], "src/status.go");
        assert_eq!(mapped["content"], "package cli\n");
    }

    // NEGATIVE: shell listing commands stay on `run` — the HOST owns any
    // structured-listing ergonomics (which listing tool exists, how recursion
    // maps onto it), so this layer must pass the command through untouched.
    #[test]
    fn leaves_shell_listing_run_commands_untouched() {
        let args = serde_json::json!({"command": "ls -R"});
        let (name, mapped) = normalize_tool_call_shape("run", args.clone());
        assert_eq!(name, "run");
        assert_eq!(mapped, args);

        let args = serde_json::json!({"command": "tree -L 2 src"});
        let (name, mapped) = normalize_tool_call_shape("run", args.clone());
        assert_eq!(name, "run");
        assert_eq!(mapped, args);
    }

    // NEGATIVE: `replace_symbol` is a hard-kept STANDALONE tool in Harn's default
    // surface, so a top-level call is legitimate and must NOT be folded into
    // `edit` (which would shadow the real tool). `remove_symbol` is excluded for
    // the same symbol-tool symmetry.
    #[test]
    fn does_not_alias_symbol_edit_tools() {
        let (name, mapped) =
            normalize_tool_call_shape("replace_symbol", serde_json::json!({"symbol": "Foo"}));
        assert_eq!(name, "replace_symbol");
        assert!(mapped.get("action").is_none());

        let (name, _) = normalize_tool_call_shape("remove_symbol", serde_json::json!({}));
        assert_eq!(name, "remove_symbol");
    }

    // NEGATIVE: raw-write / whole-file tools are semantically lossy against
    // `edit` (no raw-write semantics), so they must NOT be silently rewritten.
    // Leaving them unmapped lets the actionable denial feedback guide the model.
    #[test]
    fn does_not_alias_write_file_to_edit() {
        let args = serde_json::json!({"path": "README.md", "content": "hi"});
        let (name, mapped) = normalize_tool_call_shape("write_file", args.clone());
        assert_eq!(name, "write_file");
        assert_eq!(mapped, args);
    }

    #[test]
    fn does_not_alias_delete_file_or_patch_file() {
        let (name, _) = normalize_tool_call_shape("delete_file", serde_json::json!({}));
        assert_eq!(name, "delete_file");
        let (name, _) = normalize_tool_call_shape("patch_file", serde_json::json!({}));
        assert_eq!(name, "patch_file");
    }
}
