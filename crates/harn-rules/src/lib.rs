//! # harn-rules
//!
//! The declarative structural rule engine for Harn — the Rust core that
//! powers `harn rules` / lint / codemod surfaces. A **rule** describes
//! *what to match* (a pattern snippet, a node kind, or a regex) and
//! optionally *how to rewrite* it (a `fix`); the engine compiles that rule
//! against the tree-sitter machinery in [`harn_hostlib::ast`] and produces
//! [`RuleMatch`]es with metavariable bindings.
//!
//! This crate delivers the **atomic matching tier** (issue #2832):
//!
//! - [`model`] — the serde rule data model (`id` / `language` / `severity`
//!   / `message` / `rule` block / `fix`).
//! - [`pattern`] — the snippet → tree-sitter-query compiler (`$VAR`
//!   metavariable lifting + unification).
//! - [`engine`] — compile a [`Rule`] and run it to produce matches.
//! - [`loader`] — load rules from a TOML file or a directory.
//!
//! Relational/composite matching (#2833) and `where` / `transform` / `fix`
//! interpolation (#2834) layer onto this surface.
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

pub mod engine;
pub mod error;
pub mod loader;
pub mod model;
pub mod pattern;

pub use engine::{Binding, CompiledRule, RuleMatch, Span};
pub use error::RulesError;
pub use loader::{load_rule_dir, load_rule_file};
pub use model::{AtomicMatcher, Matcher, Rule, RuleKind, Severity};
pub use pattern::{compile_pattern, CompiledPattern};
