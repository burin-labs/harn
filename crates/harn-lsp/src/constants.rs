use std::collections::BTreeMap;
use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub(crate) struct BuiltinDetail {
    pub(crate) name: String,
    pub(crate) signature: String,
    doc: Option<String>,
}

/// Canonical documentation for one builtin, keyed by name.
/// Built once from the `#[harn_builtin]` descriptor slice
/// ([`harn_vm::stdlib::all_builtin_defs`]) so it is immune to the
/// registration-order race that motivated harn#2588: every `#[harn_builtin]`
/// fn contributes its authored docs regardless of registration order.
fn canonical_builtin_docs() -> &'static BTreeMap<&'static str, Option<&'static str>> {
    static CANONICAL: OnceLock<BTreeMap<&'static str, Option<&'static str>>> = OnceLock::new();
    CANONICAL.get_or_init(|| {
        let mut map = BTreeMap::new();
        for def in harn_vm::stdlib::all_builtin_defs() {
            for name in std::iter::once(def.sig.name).chain(def.aliases.iter().copied()) {
                map.entry(name).or_insert(def.doc);
            }
        }
        map
    })
}

pub(crate) fn builtin_details() -> &'static [BuiltinDetail] {
    static DETAILS: OnceLock<Vec<BuiltinDetail>> = OnceLock::new();
    DETAILS.get_or_init(|| {
        // The metadata probe installs the complete immutable manifest before
        // the parser answers which globals are source-callable.
        let runtime_metadata = harn_vm::stdlib::stdlib_builtin_metadata();
        let canonical = canonical_builtin_docs();
        let mut details: Vec<BuiltinDetail> = harn_parser::builtin_signatures::iter_builtin_names()
            .filter_map(|name| {
                let signature = harn_parser::builtin_signatures::lookup(name)?;
                let doc = canonical
                    .get(name)
                    .and_then(|doc| *doc)
                    .or_else(|| {
                        runtime_metadata
                            .iter()
                            .find(|metadata| metadata.name() == name)
                            .and_then(|metadata| metadata.doc())
                    })
                    .map(str::to_string);
                Some(BuiltinDetail {
                    name: name.to_string(),
                    signature: format_builtin_signature(name, signature),
                    doc,
                })
            })
            .collect();
        details.sort_by(|left, right| left.name.cmp(&right.name));
        details
    })
}

pub(crate) fn capability_method_details(field: &str) -> &'static [BuiltinDetail] {
    static DETAILS: OnceLock<BTreeMap<&'static str, Vec<BuiltinDetail>>> = OnceLock::new();
    DETAILS
        .get_or_init(|| {
            // Reuse the global projection's one-time metadata probe so opening
            // Harness completion never constructs a second probe VM.
            let _ = builtin_details();
            harn_builtin_meta::CapabilityId::ALL
                .iter()
                .map(|capability| {
                    let field = capability.field_name();
                    let methods =
                        harn_capability_contracts::declared_capability_method_names(field)
                            .into_iter()
                            .map(|method| {
                                let entry =
                                    harn_parser::builtin_signatures::capability_method_entry(
                                        field, method,
                                    );
                                BuiltinDetail {
                                    name: method.to_string(),
                                    signature: entry.map_or_else(
                                        || format!("{method}(…)"),
                                        |entry| format_builtin_signature(method, entry.signature),
                                    ),
                                    doc: authored_capability_method_doc(*capability, method),
                                }
                            })
                            .collect();
                    (field, methods)
                })
                .collect()
        })
        .get(field)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn authored_capability_method_doc(
    capability: harn_builtin_meta::CapabilityId,
    method: &str,
) -> Option<String> {
    use harn_builtin_meta::BuiltinExposure;

    let owns_method = |exposure| {
        matches!(
            exposure,
            BuiltinExposure::HarnessMethod {
                capability: candidate,
                method: candidate_method,
            } if candidate == capability && candidate_method == method
        )
    };
    harn_capability_contracts::ALL_CAPABILITY_METHOD_DEFS
        .iter()
        .find(|def| owns_method(def.contract.exposure))
        .map(|def| def.doc.to_string())
        .or_else(|| {
            harn_vm::stdlib::all_builtin_defs()
                .iter()
                .find(|def| owns_method(def.contract.exposure))
                .and_then(|def| def.doc.map(str::to_string))
        })
}

fn format_builtin_signature(
    name: &'static str,
    signature: &harn_parser::builtin_signatures::BuiltinSignature,
) -> String {
    signature.with_name(name).to_string()
}

pub(crate) fn builtin_doc(name: &str) -> Option<String> {
    builtin_details()
        .iter()
        .find(|detail| detail.name == name)
        .map(|detail| {
            detail.doc.as_ref().map_or_else(
                || format!("**{}**", detail.signature),
                |doc| format!("**{}** — {doc}", detail.signature),
            )
        })
}

pub(crate) fn capability_method_doc(field: &str, method: &str) -> Option<String> {
    capability_method_details(field)
        .iter()
        .find(|detail| detail.name == method)
        .map(|detail| {
            detail.doc.as_ref().map_or_else(
                || format!("**{}**", detail.signature),
                |doc| format!("**{}** — {doc}", detail.signature),
            )
        })
}

pub(crate) fn builtin_signature(name: &str) -> Option<&'static str> {
    builtin_details()
        .iter()
        .find(|detail| detail.name.as_str() == name)
        .map(|detail| detail.signature.as_str())
}

pub(crate) fn is_builtin(name: &str) -> bool {
    builtin_signature(name).is_some()
}

/// Known keywords for completion, owned by the lexer token table.
pub(crate) use harn_lexer::KEYWORDS;

/// String methods offered after `.` on a string value.
pub(crate) const STRING_METHODS: &[&str] = &[
    "count",
    "empty",
    "trim",
    "split",
    "contains",
    "starts_with",
    "ends_with",
    "replace",
    "uppercase",
    "lowercase",
    "substring",
    "index_of",
    "chars",
    "repeat",
    "reversed",
    "pad_left",
    "pad_right",
];

/// List methods offered after `.` on a list value.
pub(crate) const LIST_METHODS: &[&str] = &[
    "count",
    "empty",
    "appending",
    "dropping_last",
    "map",
    "filter",
    "reduce",
    "find",
    "any",
    "all",
    "contains",
    "index_of",
    "join",
    "sorted",
    "sorted_by",
    "reversed",
    "flat_map",
    "flatten",
    "slice",
    "enumerate",
    "zip",
    "unique",
    "take",
    "skip",
    "sum",
    "min",
    "max",
];

/// Dict methods offered after `.` on a dict value.
pub(crate) const DICT_METHODS: &[&str] = &[
    "keys",
    "values",
    "entries",
    "count",
    "has",
    "merging",
    "map_values",
    "filter",
    "removing",
    "get",
];

/// Known type names used after `:` in type annotations.
pub(crate) const TYPE_NAMES: &[&str] = &[
    "int",
    "float",
    "decimal",
    "string",
    "bool",
    "nil",
    "list",
    "dict",
    "iter",
    "Generator",
    "generator",
    "Stream",
    "stream",
    "any",
    "void",
    "channel",
    "atomic",
    "mutex",
    "closure",
];

pub(crate) fn keyword_doc(name: &str) -> Option<String> {
    let doc = match name {
        "pipeline" => "**pipeline** — Declare a named pipeline\n\n```harn\npipeline name(params) {\n  // body\n}\n```",
        "fn" => "**fn** — Declare a function\n\n```harn\nfn name(params) -> return_type {\n  // body\n}\n```",
        "const" => "**const** — Immutable variable binding\n\n```harn\nconst x: type = value\n```",
        "let" => "**let** — Mutable variable binding\n\n```harn\nlet x: type = value\n```",
        "if" => "**if** — Conditional expression\n\n```harn\nif condition {\n  // then\n} else {\n  // else\n}\n```",
        "else" => "**else** — Else branch of an if expression",
        "for" => "**for** — For-in loop\n\n```harn\nfor item in iterable {\n  // body\n}\n```",
        "while" => "**while** — While loop\n\n```harn\nwhile condition {\n  // body\n}\n```",
        "match" => "**match** — Pattern matching expression\n\n```harn\nmatch value {\n  pattern => body\n}\n```",
        "require" => "**require** — Runtime precondition check\n\n```harn\nrequire condition, \"message\"\n```\n\nThrows if the condition is false.",
        "return" => "**return** — Return a value from a function",
        "try" => "**try** — Try-catch error handling\n\n```harn\ntry {\n  // body\n} catch e {\n  // handle\n}\n```",
        "catch" => "**catch** — Catch block for error handling",
        "throw" => "**throw** — Throw an error value",
        "import" => "**import** — Import a module\n\n```harn\nimport \"path/to/module\"\n```",
        "spawn" => "**spawn** — Spawn an async task\n\n```harn\nlet handle = spawn {\n  // async body\n}\n```",
        "parallel" => "**parallel** — Parallel execution\n\n```harn\nparallel N { i -> ... }       // count mode\nparallel each list { x -> ... } // map mode\nparallel settle list { x -> ... } // settle mode\n```",
        "defer" => "**defer** — Run body at scope exit\n\n```harn\ndefer {\n  cleanup()\n}\n```",
        "retry" => "**retry** — Retry a block N times\n\n```harn\nretry N {\n  // body\n}\n```",
        "extends" => "**extends** — Inherit from another pipeline",
        "override" => "**override** — Override an inherited pipeline step",
        "true" | "false" => "**bool** — Boolean literal",
        "nil" => "**nil** — Nil value (absence of a value)",
        "in" => "**in** — Used in `for x in collection`",
        _ => return None,
    };
    Some(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::{builtin_doc, builtin_signature, capability_method_details, is_builtin};

    #[test]
    fn lsp_registry_excludes_runtime_metadata_only_names() {
        assert!(!is_builtin("provider_capabilities"));
        assert!(!is_builtin("llm_available_providers"));
        assert!(builtin_doc("provider_capabilities").is_none());
    }

    #[test]
    fn migrated_llm_names_are_harness_methods_only() {
        assert!(builtin_signature("llm_healthcheck").is_none());
        let methods = capability_method_details("llm");
        assert!(methods.iter().any(|detail| detail.name == "healthcheck"));
        assert!(methods
            .iter()
            .any(|detail| detail.name == "provider_capabilities"));
        assert!(methods
            .iter()
            .any(|detail| detail.name == "available_providers"));
    }

    #[test]
    fn harness_method_details_project_the_typed_contract() {
        let call = capability_method_details("llm")
            .iter()
            .find(|detail| detail.name == "call")
            .expect("harness.llm.call completion");
        assert!(call.signature.starts_with("call(prompt: string"));
        assert!(call
            .doc
            .as_deref()
            .is_some_and(|doc| doc.contains("model call")));
    }
}
