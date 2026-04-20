use crate::ast::TypeExpr;

/// Statically-known return type hint for a builtin. `None` on [`BuiltinSig`]
/// means "recognized builtin, return type is dynamic/polymorphic at the parse site".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuiltinReturn {
    /// Simple named type: `"string"`, `"int"`, `"bool"`, `"nil"`, `"list"`,
    /// `"dict"`, `"float"`.
    Named(&'static str),
    /// Union of two or more named types (e.g. `["string", "nil"]` for
    /// `env` / `regex_match`).
    Union(&'static [&'static str]),
    /// The bottom type (never returns normally).
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinMetadata {
    pub name: &'static str,
    pub return_types: &'static [&'static str],
}

/// One entry in the builtin registry.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BuiltinSig {
    pub name: &'static str,
    pub return_type: Option<BuiltinReturn>,
}

/// A generic signature for a builtin: parameter types (with `Schema<T>`
/// markers) and a return type, both open over the listed type parameters.
///
/// The type checker materializes one of these at each call site, binds the
/// type parameters by walking the arg AST (so e.g. `Schema<T>` in a param
/// position pulls `T` from the value of the schema argument), and applies
/// the bindings to the return type. This replaces the per-builtin
/// `extract_llm_schema_from_options` / `schema_type_expr_from_node`
/// special cases that used to live in the `FunctionCall` arm of
/// `infer_type`.
#[derive(Debug, Clone)]
pub(crate) struct BuiltinGenericSig {
    pub type_params: Vec<String>,
    pub params: Vec<TypeExpr>,
    pub return_type: TypeExpr,
}

pub(crate) const UNION_STRING_NIL: &[&str] = &["string", "nil"];
pub(crate) const UNION_DICT_NIL: &[&str] = &["dict", "nil"];
pub(crate) const EMPTY_RETURN_TYPES: &[&str] = &[];
pub(crate) const RETURN_BOOL: &[&str] = &["bool"];
pub(crate) const RETURN_BYTES: &[&str] = &["bytes"];
pub(crate) const RETURN_DICT: &[&str] = &["dict"];
pub(crate) const RETURN_FLOAT: &[&str] = &["float"];
pub(crate) const RETURN_INT: &[&str] = &["int"];
pub(crate) const RETURN_LIST: &[&str] = &["list"];
pub(crate) const RETURN_NEVER: &[&str] = &["never"];
pub(crate) const RETURN_NIL: &[&str] = &["nil"];
pub(crate) const RETURN_STRING: &[&str] = &["string"];
