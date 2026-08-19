//! How far a single-file name analysis can be trusted.
//!
//! `ast.undefined_names` subtracts the names a file defines from the names it
//! references. That subtraction is only evidence of a defect when the analysis
//! could actually see every way a name might arrive. Two things break it, and
//! they break it for different reasons:
//!
//! - **The language.** In Python, JavaScript and TypeScript every name must be
//!   imported or bound in the same file, so a single file is a complete unit.
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

use std::sync::Arc;

use harn_vm::VmValue;
use tree_sitter::{Node, Tree};

use crate::tools::args::{build_dict, str_value};

use super::language::Language;
use super::undefined_names::node_text;

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
        | Language::Tsx => ResolutionCeiling::SingleFileComplete,
        Language::Go => ResolutionCeiling::PackageScoped,
        Language::Ruby => ResolutionCeiling::RuntimeResolved,
        _ => ResolutionCeiling::RuntimeResolved,
    }
}

/// A construct in the file that defeats resolution, named so a caller can say
/// why a finding was demoted rather than just that it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Defeater {
    WildcardImport,
    DynamicNameAccess,
    DynamicMethodDefinition,
    Eval,
    WithScope,
    SyntaxError,
}

impl Defeater {
    fn as_str(self) -> &'static str {
        match self {
            Self::WildcardImport => "wildcard_import",
            Self::DynamicNameAccess => "dynamic_name_access",
            Self::DynamicMethodDefinition => "dynamic_method_definition",
            Self::Eval => "eval",
            Self::WithScope => "with_scope",
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
            ("defeaters", VmValue::List(Arc::new(defeaters))),
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
    if tree.root_node().has_error() {
        note(Defeater::SyntaxError);
    }
    scan(tree.root_node(), source, language, &mut note);
    Resolution {
        ceiling: ceiling(language),
        defeaters,
        analysed: true,
    }
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

fn scan(node: Node<'_>, source: &str, language: Language, note: &mut impl FnMut(Defeater)) {
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
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        scan(child, source, language, note);
    }
}

/// The bare callee name of a call node, or `None` when the callee is anything
/// other than a plain identifier. A method call such as `obj.setattr(...)` is
/// not the builtin and must not be treated as one.
fn called_name<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    let callee = node
        .child_by_field_name("function")
        .or_else(|| node.child_by_field_name("method"))?;
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
    fn plain_python_file_resolves_completely() {
        let resolution =
            assess_source("import os\n\n\ndef f():\n    return os\n", Language::Python);
        assert!(resolution.is_complete());
        assert!(defeater_names(&resolution).is_empty());
    }

    #[test]
    fn python_wildcard_import_defeats_resolution() {
        let resolution = assess_source(
            "from constants import *\n\n\ndef f():\n    return MAX\n",
            Language::Python,
        );
        assert!(!resolution.is_complete());
        assert_eq!(defeater_names(&resolution), vec!["wildcard_import"]);
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
