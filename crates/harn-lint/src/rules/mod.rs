//! Source-aware lint rules that operate on raw source plus the AST,
//! rather than on the stateful [`Linter`][crate::linter::Linter] walk.
//! Each rule lives in its own submodule and is wrapped as a registry
//! entry in [`crate::rule`]; the linter dispatches by iterating the
//! registry rather than calling each rule by name.

pub(crate) mod ast_walk;
pub(crate) mod blank_lines;
pub(crate) mod deprecated_llm_options;
pub(crate) mod file_header;
pub(crate) mod import_order;
pub(crate) mod nil_coalesce;
pub(crate) mod optional_shorthand;
pub(crate) mod reminder_lifecycle;
pub(crate) mod reminder_provider_count;
pub(crate) mod reminder_role_hint;
pub(crate) mod template_provider_identity;
pub(crate) mod template_variant_explosion;
pub(crate) mod trailing_comma;
pub(crate) mod unnecessary_parentheses;
pub(crate) mod unnormalized_options;
