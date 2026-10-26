//! How far a single-file name analysis can be trusted.
//!
//! `ast.undefined_names` subtracts the names a file defines from the names it
//! references. That subtraction is only evidence of a defect when the analysis
//! could actually see every way a name might arrive. Two things break it, and
//! they break it for different reasons:
//!
//! - **The language.** In Python, JavaScript, TypeScript and Rust every name
//!   must be imported or bound in the same file, so a single file is a
//!   complete unit (Rust paths such as `a::b` are never counted as references).
//!   In Go a sibling file in the same package contributes names with no import
//!   at all. In Ruby, names are routinely created at runtime by the framework.
//! - **The file.** A wildcard import, an `eval`, a `setattr`, or a
//!   `method_missing` defeats resolution even in a language that is otherwise a
//!   complete unit.
//!
//! Both halves are required. Reporting only the language would call a Python
//! star-import file trustworthy; reporting only the file would call a Ruby file
//! trustworthy because it happens to contain no dynamic construct.
//!
//! The output is a fact about the analysis, not a verdict about the code.
//! Callers decide what to do with a low-confidence finding; this module only
//! refuses to overstate one.

use std::collections::HashSet;

use harn_vm::VmValue;
use tree_sitter::{Node, Tree};

use crate::tools::args::{build_dict, str_value};

use super::language::Language;
use super::undefined_names::{bind_rust_use_tree, node_text};

/// How far single-file name resolution can be trusted for a language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResolutionCeiling {
    /// Every name must be imported or bound in this file, so one file is a
    /// complete unit and a clean file yields confident findings.
    SingleFileComplete,
    /// Sibling files in the same package or namespace contribute names without
    /// an import, so one file can never see the whole symbol space.
    PackageScoped,
    /// Names are routinely created at runtime, so an unresolved name is not
    /// evidence of a defect.
    RuntimeResolved,
}

impl ResolutionCeiling {
    fn as_str(self) -> &'static str {
        match self {
            Self::SingleFileComplete => "single_file_complete",
            Self::PackageScoped => "package_scoped",
            Self::RuntimeResolved => "runtime_resolved",
        }
    }
}

/// The declared ceiling per language. Data, not a branch at a call site.
pub(super) fn ceiling(language: Language) -> ResolutionCeiling {
    match language {
        Language::Python
        | Language::JavaScript
        | Language::Jsx
        | Language::TypeScript
        | Language::Tsx
        | Language::Rust => ResolutionCeiling::SingleFileComplete,
        Language::Go => ResolutionCeiling::PackageScoped,
        Language::Ruby => ResolutionCeiling::RuntimeResolved,
        _ => ResolutionCeiling::RuntimeResolved,
    }
}

/// A construct in the file that defeats resolution, named so a caller can say
/// why a finding was demoted rather than just that it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Defeater {
    WildcardImport,
    DynamicNameAccess,
    DynamicMethodDefinition,
    Eval,
    WithScope,
    IdentifierNormalization,
    SyntaxError,
}

impl Defeater {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::WildcardImport => "wildcard_import",
            Self::DynamicNameAccess => "dynamic_name_access",
            Self::DynamicMethodDefinition => "dynamic_method_definition",
            Self::Eval => "eval",
            Self::WithScope => "with_scope",
            Self::IdentifierNormalization => "identifier_normalization",
            Self::SyntaxError => "syntax_error",
        }
    }
}

/// The assessed resolvability of one file.
#[derive(Debug, Clone)]
pub(super) struct Resolution {
    ceiling: ResolutionCeiling,
    defeaters: Vec<Defeater>,
    /// False when no analysis ran at all — an unsupported language, or a file
    /// that did not parse. Kept separate from `defeaters` so "we looked and
    /// found nothing" can never be confused with "we did not look".
    analysed: bool,
}

impl Resolution {
    /// A finding is confident only when the analysis ran, the language allows
    /// it, and the file did not defeat it.
    pub(super) fn is_complete(&self) -> bool {
        self.analysed
            && self.ceiling == ResolutionCeiling::SingleFileComplete
            && self.defeaters.is_empty()
    }

    pub(super) fn to_vm_value(&self) -> VmValue {
        let defeaters: Vec<VmValue> = self
            .defeaters
            .iter()
            .map(|d| str_value(d.as_str()))
            .collect();
        build_dict([
            ("complete", VmValue::Bool(self.is_complete())),
            ("analysed", VmValue::Bool(self.analysed)),
            ("ceiling", str_value(self.ceiling.as_str())),
            ("defeaters", VmValue::list(defeaters)),
        ])
    }

    /// The reading for a file we could not analyse at all. It is never
    /// complete: no analysis ran, so nothing was ruled out.
    pub(super) fn unanalysed(language: Language) -> Self {
        Self {
            ceiling: ceiling(language),
            defeaters: Vec::new(),
            analysed: false,
        }
    }
}

/// Assess one parsed file.
pub(super) fn assess(tree: &Tree, source: &str, language: Language) -> Resolution {
    Resolution {
        ceiling: ceiling(language),
        defeaters: binding_defeaters(tree, source, language),
        analysed: true,
    }
}

/// Constructs that can supply bindings absent from the parsed source. Reads
/// demote confidence; mutations must refuse before persisting their plan.
pub(crate) fn binding_defeaters(tree: &Tree, source: &str, language: Language) -> Vec<Defeater> {
    binding_defeaters_at(tree.root_node(), source, language)
}

/// Bindings supplied outside a subtree still affect the names it uses.
pub(crate) fn binding_defeaters_at(
    node: Node<'_>,
    source: &str,
    language: Language,
) -> Vec<Defeater> {
    let root = source_root(node);
    let mut defeaters: Vec<Defeater> = Vec::new();
    let mut note = |d: Defeater| {
        if !defeaters.contains(&d) {
            defeaters.push(d);
        }
    };
    // tree-sitter recovers from syntax errors rather than refusing to parse, so
    // a broken file still yields a tree. Definitions inside the damaged region
    // may never have been collected, which turns every name they would have
    // bound into a spurious undefined-name finding. A file that does not parse
    // cleanly cannot support a confident reading.
    if root.has_error() {
        note(Defeater::SyntaxError);
    }
    walk_nodes(root, &mut |node| {
        scan_node(node, source, language, &mut note)
    });
    defeaters
}

/// A Python class body consults a namespace that its metaclass can populate.
/// Method bodies skip that namespace, while method defaults and annotations
/// are still evaluated in it. Base expressions precede namespace creation.
pub(crate) fn has_dynamic_class_lookup(node: Node<'_>, language: Language) -> bool {
    if language != Language::Python {
        return false;
    }
    let at = node.start_byte();
    let mut in_function_body = false;
    let mut outer = node.parent();
    while let Some(parent) = outer {
        let in_body = parent
            .child_by_field_name("body")
            .is_some_and(|body| body.byte_range().contains(&at));
        if matches!(parent.kind(), "function_definition" | "lambda") && in_body {
            in_function_body = true;
        }
        if parent.kind() == "class_definition" && in_body && !in_function_body {
            return true;
        }
        outer = parent.parent();
    }
    false
}

/// Names whose call means the file can create or read bindings the parse cannot
/// see. `getattr` is deliberately absent: reading an attribute off an object
/// does not introduce a module-level name.
const PYTHON_DYNAMIC_CALLS: &[&str] = &[
    "eval", "exec", "globals", "locals", "setattr", "delattr", "vars", "compile",
];

const RUBY_DYNAMIC_CALLS: &[&str] = &[
    "method_missing",
    "define_method",
    "const_missing",
    "instance_variable_set",
    "const_set",
    "class_eval",
    "instance_eval",
    "attr_accessor",
    "attr_reader",
    "attr_writer",
];

fn source_root(mut node: Node<'_>) -> Node<'_> {
    while let Some(parent) = node.parent() {
        node = parent;
    }
    node
}

fn walk_nodes(root: Node<'_>, visit: &mut impl FnMut(Node<'_>)) {
    let mut cursor = root.walk();
    loop {
        visit(cursor.node());
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

fn scan_node(node: Node<'_>, source: &str, language: Language, note: &mut impl FnMut(Defeater)) {
    if language
        .rename_identifier_kinds()
        .is_some_and(|kinds| kinds.contains(&node.kind()))
    {
        // The index records raw spellings. Python/Rust Unicode normalization
        // and escaped identifiers can hide a same-named binding from it.
        let spelling = node_text(node, source);
        let opaque = match language {
            Language::Python => !spelling.is_ascii(),
            Language::Rust => !spelling.is_ascii() || spelling.starts_with("r#"),
            Language::JavaScript | Language::Jsx | Language::TypeScript | Language::Tsx => {
                spelling.contains('\\')
            }
            Language::Swift => spelling.starts_with('`'),
            _ => false,
        };
        if opaque {
            note(Defeater::IdentifierNormalization);
        }
    }
    match (language, node.kind()) {
        (Language::Python, "wildcard_import") => note(Defeater::WildcardImport),
        (Language::Python, "call") => {
            if let Some(name) = called_name(node, source) {
                if PYTHON_DYNAMIC_CALLS.contains(&name) {
                    note(if name == "eval" || name == "exec" || name == "compile" {
                        Defeater::Eval
                    } else {
                        Defeater::DynamicNameAccess
                    });
                }
            }
        }
        (Language::Python, "function_definition") => {
            if let Some(name) = node.child_by_field_name("name") {
                if matches!(node_text(name, source), "__getattr__" | "__getattribute__") {
                    note(Defeater::DynamicNameAccess);
                }
            }
        }
        (Language::Ruby, "call") => {
            if let Some(name) = called_name(node, source) {
                if RUBY_DYNAMIC_CALLS.contains(&name) {
                    note(Defeater::DynamicMethodDefinition);
                }
            }
        }
        (Language::Ruby, "method") => {
            if let Some(name) = node.child_by_field_name("name") {
                if matches!(node_text(name, source), "method_missing" | "const_missing") {
                    note(Defeater::DynamicMethodDefinition);
                }
            }
        }
        (
            Language::JavaScript | Language::Jsx | Language::TypeScript | Language::Tsx,
            "call_expression",
        ) => {
            if let Some(name) = called_name(node, source) {
                if name == "eval" {
                    note(Defeater::Eval);
                }
            }
        }
        (
            Language::JavaScript | Language::Jsx | Language::TypeScript | Language::Tsx,
            "with_statement",
        ) => note(Defeater::WithScope),
        (Language::Rust, "use_wildcard") => note(Defeater::WildcardImport),
        (Language::Rust, "macro_invocation" | "attribute_item" | "inner_attribute_item")
            if rust_expansion_may_bind(node, source) =>
        {
            note(Defeater::DynamicNameAccess);
        }
        (Language::Swift, "macro_invocation" | "attribute") => note(Defeater::DynamicNameAccess),
        _ => {}
    }
}

/// `std` macros that expand to an expression or statement and declare no item.
/// `include!` and `thread_local!` are absent because they do declare items.
const RUST_ITEMLESS_STD_MACROS: &[&str] = &[
    "assert",
    "assert_eq",
    "assert_ne",
    "cfg",
    "column",
    "compile_error",
    "concat",
    "dbg",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "env",
    "eprint",
    "eprintln",
    "file",
    "format",
    "format_args",
    "include_bytes",
    "include_str",
    "line",
    "matches",
    "module_path",
    "option_env",
    "panic",
    "print",
    "println",
    "stringify",
    "todo",
    "unimplemented",
    "unreachable",
    "vec",
    "write",
    "writeln",
];

/// Built-in attributes that cannot introduce an unseen binding. Expansion
/// wrappers (`derive` and `cfg_attr`) inspect their arguments separately.
/// `path` is absent because it changes which file a module loads.
const RUST_INERT_ATTRIBUTES: &[&str] = &[
    "allow",
    "automatically_derived",
    "cfg",
    "cold",
    "deny",
    "deprecated",
    "doc",
    "expect",
    "export_name",
    "forbid",
    "ignore",
    "inline",
    "link_section",
    "macro_export",
    "must_use",
    "no_mangle",
    "non_exhaustive",
    "repr",
    "should_panic",
    "test",
    "track_caller",
    "used",
    "warn",
];

/// Whether a Rust macro invocation or attribute can expand into bindings the
/// parse cannot see.
///
/// Only an invocation in item or statement position can declare an item or a
/// call-site `let` that sibling code sees; one nested in an expression expands
/// to that expression. Attributes other than the inert set may rewrite the
/// item they decorate.
fn rust_expansion_may_bind(node: Node<'_>, source: &str) -> bool {
    match node.kind() {
        "macro_invocation" => {
            let in_statement_position = node.parent().is_some_and(|parent| {
                matches!(
                    parent.kind(),
                    "source_file" | "declaration_list" | "block" | "expression_statement"
                )
            });
            in_statement_position && !rust_itemless_std_macro(node, source)
        }
        "attribute_item" | "inner_attribute_item" => {
            let Some(attribute) = node.named_child(0) else {
                return true;
            };
            let Some(path) = attribute.named_child(0) else {
                return true;
            };
            !rust_attribute_is_inert(
                node,
                source,
                node_text(path, source),
                attribute.child_by_field_name("arguments"),
            )
        }
        _ => false,
    }
}

fn rust_attribute_is_inert(
    site: Node<'_>,
    source: &str,
    path: &str,
    arguments: Option<Node<'_>>,
) -> bool {
    // An imported procedural attribute can replace even `derive` or `test`.
    // Prove the binding before trusting either its spelling or arguments.
    let (prefix, name) = path.split_once("::").unwrap_or(("", path));
    if !rust_macro_path_is_unshadowed(site, source, prefix, name) {
        return false;
    }
    match path {
        "derive" => {
            // Custom derives may append arbitrary items, including a local
            // type that shadows the receiver owner. Only unshadowed builtins
            // establish the narrower trait-implementation-only guarantee.
            const BUILTIN_DERIVES: &[&str] = &[
                "Clone",
                "Copy",
                "Debug",
                "Default",
                "Eq",
                "Hash",
                "Ord",
                "PartialEq",
                "PartialOrd",
            ];
            let Some(parts) = rust_attribute_arguments(arguments) else {
                return false;
            };
            let mut expect_name = true;
            for part in parts {
                if expect_name && part.kind() == "identifier" {
                    let name = node_text(part, source);
                    if !BUILTIN_DERIVES.contains(&name)
                        || !rust_macro_path_is_unshadowed(site, source, "", name)
                    {
                        return false;
                    }
                    expect_name = false;
                } else if !expect_name && part.kind() == "," {
                    expect_name = true;
                } else {
                    return false;
                }
            }
            true
        }
        "cfg_attr" => {
            let Some(parts) = rust_attribute_arguments(arguments) else {
                return false;
            };
            let mut groups = parts.split(|part| part.kind() == ",");
            // Do not guess a project's active configuration. Every possible
            // generated attribute must preserve the same binding guarantee.
            if groups.next().is_none_or(|predicate| predicate.is_empty()) {
                return false;
            }
            let mut seen = false;
            for group in groups {
                if group.is_empty() {
                    continue;
                }
                seen = true;
                let path = node_text(group[0], source);
                let arguments = match group.get(1) {
                    Some(node) if node.kind() == "token_tree" && group.len() == 2 => Some(*node),
                    _ if matches!(path, "derive" | "cfg_attr") => return false,
                    _ => None,
                };
                if group[0].kind() != "identifier"
                    || group.get(1).is_some_and(|node| node.kind() == "::")
                    || !rust_attribute_is_inert(site, source, path, arguments)
                {
                    return false;
                }
            }
            seen
        }
        _ => {
            RUST_INERT_ATTRIBUTES.contains(&path)
                || path.split_once("::").is_some_and(|(prefix, _)| {
                    matches!(prefix, "clippy" | "rustfmt" | "diagnostic")
                })
        }
    }
}

fn rust_attribute_arguments(arguments: Option<Node<'_>>) -> Option<Vec<Node<'_>>> {
    let arguments = arguments?;
    let count = arguments.child_count();
    if count < 2 || arguments.child(0)?.kind() != "(" || arguments.child(count - 1)?.kind() != ")" {
        return None;
    }
    Some(
        (1..count - 1)
            .filter_map(|index| arguments.child(index))
            .filter(|node| !matches!(node.kind(), "line_comment" | "block_comment"))
            .collect(),
    )
}

/// Whether a Rust `macro_invocation` names an itemless `std` macro, bare or
/// through `std`, `core` or `alloc`.
pub(crate) fn rust_itemless_std_macro(invocation: Node<'_>, source: &str) -> bool {
    let Some(path) = invocation.child_by_field_name("macro") else {
        return false;
    };
    let path = node_text(path, source);
    let (prefix, name) = path.rsplit_once("::").unwrap_or(("", path));
    let prefix = prefix.trim_start_matches("::");
    matches!(prefix, "" | "std" | "core" | "alloc")
        && RUST_ITEMLESS_STD_MACROS.contains(&name)
        && rust_macro_path_is_unshadowed(invocation, source, prefix, name)
}

fn rust_macro_path_is_unshadowed(
    invocation: Node<'_>,
    source: &str,
    prefix: &str,
    name: &str,
) -> bool {
    let root = source_root(invocation);
    let binding = if prefix.is_empty() { name } else { prefix };
    let mut shadowed = false;
    // Without macro resolution, a competing binding anywhere in the file
    // defeats the standard-library spelling proof.
    walk_nodes(root, &mut |node| match node.kind() {
        "macro_definition" if prefix.is_empty() => {
            shadowed |= node
                .child_by_field_name("name")
                .is_some_and(|name| node_text(name, source) == binding);
        }
        "mod_item" | "extern_crate_declaration" if !prefix.is_empty() => {
            shadowed |= ["name", "alias"].into_iter().any(|field| {
                node.child_by_field_name(field)
                    .is_some_and(|name| node_text(name, source) == binding)
            });
        }
        "use_declaration" => {
            let mut bindings = HashSet::new();
            if let Some(argument) = node.child_by_field_name("argument") {
                bind_rust_use_tree(argument, source, &mut bindings);
            }
            shadowed |= bindings.contains(binding);
            walk_nodes(node, &mut |part| shadowed |= part.kind() == "use_wildcard");
        }
        "attribute_item" | "inner_attribute_item" if prefix.is_empty() => {
            shadowed |= node
                .named_child(0)
                .and_then(|attribute| attribute.named_child(0))
                .is_some_and(|path| node_text(path, source) == "macro_use");
        }
        _ => {}
    });
    !shadowed
}

/// The bare callee name of a call node, or `None` when the callee is anything
/// other than a plain identifier. A method call such as `obj.setattr(...)` is
/// not the builtin and must not be treated as one.
fn called_name<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    let mut callee = node
        .child_by_field_name("function")
        .or_else(|| node.child_by_field_name("method"))?;
    while callee.kind() == "parenthesized_expression" {
        let mut cursor = callee.walk();
        callee = callee
            .named_children(&mut cursor)
            .find(|child| !child.is_extra())?;
    }
    match callee.kind() {
        "identifier" => Some(node_text(callee, source)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::parse::parse_source;

    fn assess_source(source: &str, language: Language) -> Resolution {
        let tree = parse_source(source, language).expect("parses");
        assess(&tree, source, language)
    }

    fn defeater_names(resolution: &Resolution) -> Vec<&'static str> {
        resolution.defeaters.iter().map(|d| d.as_str()).collect()
    }

    #[test]
    fn python_dynamic_name_calls_defeat_resolution() {
        let resolution = assess_source("def f(t):\n    setattr(t, 'a', 1)\n", Language::Python);
        assert!(!resolution.is_complete());
        assert_eq!(defeater_names(&resolution), vec!["dynamic_name_access"]);
    }

    #[test]
    fn python_module_getattr_defeats_resolution() {
        let resolution = assess_source(
            "def __getattr__(name):\n    return name\n",
            Language::Python,
        );
        assert!(!resolution.is_complete());
        assert_eq!(defeater_names(&resolution), vec!["dynamic_name_access"]);
    }

    #[test]
    fn python_method_named_like_a_builtin_is_not_a_defeater() {
        // `obj.setattr(...)` is a method call, not the builtin.
        let resolution = assess_source("def f(o):\n    o.setattr('a', 1)\n", Language::Python);
        assert!(resolution.is_complete());
    }

    #[test]
    fn ruby_is_never_complete_even_when_plainly_written() {
        let resolution = assess_source("def total(a)\n  a + 1\nend\n", Language::Ruby);
        assert!(!resolution.is_complete());
        assert_eq!(resolution.ceiling.as_str(), "runtime_resolved");
        assert!(defeater_names(&resolution).is_empty());
    }

    #[test]
    fn ruby_dynamic_definition_is_named_as_a_defeater() {
        let resolution = assess_source(
            "class A\n  def method_missing(n)\n    n\n  end\nend\n",
            Language::Ruby,
        );
        assert!(!resolution.is_complete());
        assert_eq!(
            defeater_names(&resolution),
            vec!["dynamic_method_definition"]
        );
    }

    #[test]
    fn go_is_package_scoped_because_sibling_files_contribute_names() {
        let resolution = assess_source("package main\n\nfunc f() int { return 1 }\n", Language::Go);
        assert!(!resolution.is_complete());
        assert_eq!(resolution.ceiling.as_str(), "package_scoped");
    }

    #[test]
    fn javascript_eval_defeats_resolution() {
        let resolution = assess_source("function f(s) { return eval(s); }\n", Language::JavaScript);
        assert!(!resolution.is_complete());
        assert_eq!(defeater_names(&resolution), vec!["eval"]);
    }

    #[test]
    fn parenthesized_eval_still_defeats_resolution() {
        for source in [
            "function f(s) { return ((eval))(s); }\n",
            "function f(s) { return ((/* comment */ eval))(s); }\n",
        ] {
            let resolution = assess_source(source, Language::JavaScript);
            assert!(!resolution.is_complete());
            assert_eq!(defeater_names(&resolution), vec!["eval"]);
        }
    }

    #[test]
    fn identifier_spellings_that_require_normalization_are_not_complete() {
        for (language, source) in [
            (
                Language::Python,
                "def probe(ℝeading):\n    return Reading()\n",
            ),
            (
                Language::Rust,
                "fn probe() { let r#Reading = || 7; Reading(); }\n",
            ),
            (
                Language::JavaScript,
                "function probe() { const Read\\u0069ng = () => 7; return Reading(); }\n",
            ),
        ] {
            let resolution = assess_source(source, language);
            assert!(!resolution.is_complete());
            assert_eq!(
                defeater_names(&resolution),
                vec!["identifier_normalization"]
            );
        }
    }

    #[test]
    fn rust_macro_expansion_is_not_a_complete_binding_analysis() {
        for source in [
            "fn f() { binding_macro::bind!(); Reading() }\n",
            "#[binding_macro::bind]\nfn f() { Reading() }\n",
            "fn f() { macro_rules! assert { () => { let Reading = 1; }; } assert!(); Reading }\n",
            "use other::assert; fn f() { assert!(); Reading() }\n",
            "use other::{bind as assert}; fn f() { assert!(); Reading() }\n",
            "use other::std::{self}; fn f() { std::assert!(); Reading() }\n",
            "mod std {} fn f() { std::assert!(); Reading() }\n",
            "#[macro_use] extern crate other; fn f() { assert!(); Reading() }\n",
            "#[derive(binding_macro::Install)] struct Marker; fn f() { Reading() }\n",
            "use binding_macro::Debug; #[derive(Debug)] struct Marker; fn f() { Reading() }\n",
            "#[derive(Clone::Debug)] struct Marker; fn f() { Reading() }\n",
            "#[allow::install] fn f() { Reading() }\n",
            "#[cfg_attr(all(), binding_macro::install)] fn f() { Reading() }\n",
            "#[cfg_attr(all(), derive(binding_macro::Install))] struct Marker; fn f() { Reading() }\n",
            "#[cfg_attr(all(), cfg_attr(all(), binding_macro::install))] fn f() { Reading() }\n",
            "use binding_macro::install as test; #[test] fn f() { Reading() }\n",
            "use binding_macro::install as derive; #[derive(Debug)] struct Marker; fn f() { Reading() }\n",
            "use binding_macro::install as derive; #[cfg_attr(all(), derive(Debug))] struct Marker; fn f() { Reading() }\n",
        ] {
            let resolution = assess_source(source, Language::Rust);
            assert!(!resolution.is_complete());
            assert_eq!(defeater_names(&resolution), vec!["dynamic_name_access"]);
        }
    }

    #[test]
    fn itemless_rust_macros_and_inert_attributes_keep_resolution_complete() {
        let source = "#![allow(dead_code)]\nuse std::{io::{self}, fmt as fmt_alias};\nuse std::assert_eq as check;\n#[derive(Debug, Clone)]\npub struct Reading;\n\
            #[cfg_attr(all(), allow(dead_code), cfg_attr(all(), derive(Debug, Clone)))]\nstruct Marker;\n\
            #[cfg(test)]\nmod tests {\n    #[test]\n    fn t() { assert_eq!(1, 1); println!(\"{}\", 2); }\n}\n\
            fn f() -> String { let v = vec![1]; std::format!(\"{v:?}\") }\n\
            fn g() -> usize { my_crate::count!(a, b) + 1 }\n";
        let resolution = assess_source(source, Language::Rust);
        assert!(
            defeater_names(&resolution).is_empty(),
            "{:?}",
            defeater_names(&resolution)
        );
        for source in [
            "fn f() { my_crate::bind!(); }\n",
            "fn f() { other::println!(); }\n",
            "include!(\"generated.rs\");\n",
            "#[tokio::main]\nasync fn main() {}\n",
        ] {
            assert_eq!(
                defeater_names(&assess_source(source, Language::Rust)),
                vec!["dynamic_name_access"],
                "{source}"
            );
        }
    }

    #[test]
    fn swift_expansion_is_not_a_complete_binding_analysis() {
        for source in [
            "func probe() { #declareReading(); Reading() }\n",
            "@GenerateBindings func probe() { Reading() }\n",
        ] {
            let resolution = assess_source(source, Language::Swift);
            assert_eq!(defeater_names(&resolution), vec!["dynamic_name_access"]);
        }
    }

    #[test]
    fn plain_javascript_file_resolves_completely() {
        let resolution = assess_source(
            "export function f(a) { return a + 1; }\n",
            Language::JavaScript,
        );
        assert!(resolution.is_complete());
    }

    #[test]
    fn defeaters_are_reported_once_each() {
        let resolution = assess_source(
            "from a import *\nfrom b import *\n\n\ndef f(t):\n    setattr(t, 'x', 1)\n    setattr(t, 'y', 2)\n",
            Language::Python,
        );
        assert_eq!(
            defeater_names(&resolution),
            vec!["wildcard_import", "dynamic_name_access"]
        );
    }

    #[test]
    fn unanalysed_file_is_never_complete() {
        assert!(!Resolution::unanalysed(Language::Python).is_complete());
    }

    #[test]
    fn a_file_that_does_not_parse_cleanly_is_not_complete() {
        // tree-sitter recovers rather than failing, so this still produces a
        // tree. Definitions in the damaged region were never collected, so the
        // reading must not claim confidence.
        let resolution = assess_source("def f(:\n    return zzz\n", Language::Python);
        assert!(!resolution.is_complete());
        assert!(defeater_names(&resolution).contains(&"syntax_error"));
    }
}
