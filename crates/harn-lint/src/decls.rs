//! Internal tracking records used by [`Linter`][crate::linter::Linter]
//! during the walk. These are crate-internal and should not leak into
//! the public API.

use harn_lexer::Span;

/// A variable declaration tracked during linting.
pub(crate) struct Declaration {
    pub(crate) name: String,
    pub(crate) span: Span,
    pub(crate) is_mutable: bool,
    /// True for simple `let x = ...` / `var x = ...` bindings, false for
    /// destructuring patterns. The `unused-variable` autofix only rewrites
    /// identifiers when true, since destructuring renames would need
    /// per-field spans we don't currently track.
    pub(crate) is_simple_ident: bool,
}

/// An import tracked during linting.
pub(crate) struct ImportInfo {
    pub(crate) names: Vec<String>,
    pub(crate) span: Span,
    /// True for `pub import { ... } from "..."`. The selectively listed
    /// names are part of the module's public surface, so the
    /// `unused-import` lint must not flag them as unused even if no
    /// local code references them.
    pub(crate) is_pub: bool,
}

/// A parameter declaration tracked during linting.
pub(crate) struct ParamDeclaration {
    pub(crate) name: String,
    pub(crate) span: Span,
}

/// A function declaration tracked for unused-function detection.
pub(crate) struct FnDeclaration {
    pub(crate) name: String,
    pub(crate) span: Span,
    pub(crate) is_pub: bool,
    pub(crate) is_method: bool,
}

/// A type declaration tracked for unused-type detection.
pub(crate) struct TypeDeclaration {
    pub(crate) name: String,
    pub(crate) span: Span,
    pub(crate) kind: &'static str,
}
