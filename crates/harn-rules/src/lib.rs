//! # harn-rules
//!
//! The declarative structural rule engine for Harn — the Rust core that
//! powers `harn rules` / lint / codemod surfaces. A **rule** describes
//! *what to match* (a pattern snippet, a node kind, or a regex) and
//! optionally *how to rewrite* it (a `fix`); the engine compiles that rule
//! against the tree-sitter machinery in [`harn_hostlib::ast`] and produces
//! [`RuleMatch`]es with metavariable bindings.
//!
//! This crate delivers the **atomic matching tier** (#2832), the
//! **relational + composite algebra** (#2833), the **predicate + rewrite
//! layer** (#2834), and the **safety + idempotency gate** (#2835):
//!
//! - [`model`] — the serde rule data model: the recursive [`RuleNode`]
//!   (atomic `pattern` / `kind` / `regex`, relational `inside` / `has` /
//!   `follows` / `precedes`, composite `all` / `any` / `not` / `matches`)
//!   plus `where` / `transform` / `fix` and `utils`.
//! - [`pattern`] — the snippet → tree-sitter-query compiler (`$VAR`
//!   metavariable lifting, unification, literal patterns, and typed
//!   `$VAR:kind` placeholders).
//! - [`evaluator`] — the tree-walking match algebra (relational + composite
//!   + utility-rule reuse).
//! - [`constraint`] — `where` predicates on captured metavars (regex,
//!   comparison, recursive sub-pattern).
//! - [`transform`] — synthesize new metavars (`replace` / `substring` /
//!   `convert`) before fixing.
//! - [`fix`] — `fix` template interpolation and format-preserving splice.
//! - [`engine`] — compile a [`Rule`], run it to produce matches,
//!   [`CompiledRule::apply`] / [`CompiledRule::auto_apply`] /
//!   [`CompiledRule::apply_checked`] a codemod (safety-gated, idempotency
//!   checked), and emit [`Diagnostic`]s.
//! - [`recipe`] — the whole-project scan → accumulate → edit lifecycle,
//!   with file create/delete ([`ScanningRecipe`], [`run_recipe`]).
//! - [`report`] — report-only [`DataTable`]s: columnar findings + metrics.
//! - [`loader`] — load rules from a TOML file or a directory.
//!
//! The whole-project scan lifecycle (#2836) layers onto this surface.
//!
//! ```
//! use harn_rules::{Rule, CompiledRule};
//!
//! let rule = Rule::from_toml_str(
//!     r#"
//!     id = "destructure-default"
//!     language = "typescript"
//!     fix = "{ $KEY: $SRC }"
//!     [rule]
//!     pattern = "$SRC?.$KEY ?? $DEFAULT"
//!     "#,
//! ).unwrap();
//! let compiled = CompiledRule::compile(&rule).unwrap();
//! let matches = compiled.run("const a = cfg?.timeout ?? 30;").unwrap();
//! assert_eq!(matches[0].bindings["KEY"].text, "timeout");
//! ```

#![forbid(unsafe_code)]

pub mod constraint;
pub mod engine;
pub mod error;
pub mod evaluator;
pub mod fix;
pub mod fold;
pub mod loader;
pub mod model;
pub mod pattern;
pub mod recipe;
pub mod report;
pub mod testing;
pub mod transform;

pub use engine::{Binding, CodemodResult, CompiledRule, Diagnostic, RuleMatch, Span};
pub use error::RulesError;
pub use fix::{interpolate, AppliedEdit};
pub use loader::{load_rule_dir, load_rule_file};
pub use model::{
    Applicability, AtomicMatcher, Comparison, Constraint, ConvertOp, Rule, RuleKind, RuleNode,
    Safety, Severity, StopBy, StopKeyword, Transform,
};
pub use pattern::{compile_pattern, CompiledPattern};
pub use recipe::{run_recipe, FileChange, RecipeRun, RuleRecipe, ScanningRecipe, SourceFile};
pub use report::{data_table, DataTable, TableRow, TableSummary};
pub use testing::{run_inline_test, Expectation, FailureKind, InlineTestReport, TestFailure};
