//! Const-constructible type definitions for Harn builtin signatures.
//!
//! Both `harn-parser` (for typechecking) and `harn-vm` (for runtime metadata)
//! consume these shapes. Living in a dep-free crate lets the parser see the
//! types without depending on the VM, and lets the `#[harn_builtin]` proc-macro
//! emit `const` literals that link into either side.
//!
//! `Ty::to_type_expr` and friends, which convert into the parser's runtime
//! `TypeExpr`, live in `harn-parser` since they depend on parser-internal AST.

/// A complete, static description of one builtin: identifier, arity range,
/// per-parameter types, generic type parameters, return type, and any
/// where-clause bounds the type checker should enforce on call.
#[derive(Debug, Clone, Copy)]
pub struct BuiltinSignature {
    /// Builtin name as registered in the VM and referenced from Harn source.
    pub name: &'static str,
    /// Positional parameters in declaration order. Trailing entries with
    /// `optional: true` define the lower bound of the arity range; the
    /// remaining entries plus `has_rest` define the upper bound.
    pub params: &'static [Param],
    /// Statically-known return type. Use [`Ty::Any`] when the return is
    /// genuinely dynamic (e.g. `json_parse`).
    pub returns: Ty,
    /// Generic type parameter names declared on this builtin (e.g. `["T"]`
    /// for `schema_parse<T>`).
    pub type_params: &'static [&'static str],
    /// True when the final parameter is variadic (rest). When set, the
    /// effective arity upper bound is unbounded and the runtime will treat
    /// trailing args as the rest-list.
    pub has_rest: bool,
    /// `where T: Foo` constraints. Each entry binds a generic type
    /// parameter name to the name of an interface it must implement.
    pub where_clauses: &'static [(&'static str, &'static str)],
}

/// One parameter slot inside a [`BuiltinSignature`].
#[derive(Debug, Clone, Copy)]
pub struct Param {
    pub name: &'static str,
    pub ty: Ty,
    /// True when this parameter has a default at the call site (so it may
    /// be omitted). All optional params must be trailing.
    pub optional: bool,
}

impl Param {
    pub const fn new(name: &'static str, ty: Ty) -> Self {
        Self {
            name,
            ty,
            optional: false,
        }
    }

    pub const fn optional(name: &'static str, ty: Ty) -> Self {
        Self {
            name,
            ty,
            optional: true,
        }
    }
}

/// `const`-friendly type IR used in builtin descriptors. Mirrors the runtime
/// `TypeExpr` from `harn-parser` but is constructable in `const` position with
/// no allocation. Convert to `TypeExpr` at the boundary via the parser-side
/// `Ty::to_type_expr` helper.
#[derive(Debug, Clone, Copy)]
pub enum Ty {
    /// A primitive or user-defined named type: `int`, `string`, `bool`,
    /// `float`, `nil`, `bytes`, `dict`, `list`, `closure`, `duration`,
    /// `any`, etc.
    Named(&'static str),
    /// Reference to a generic type parameter declared on the enclosing
    /// signature (e.g. `Generic("T")`).
    Generic(&'static str),
    /// Untyped/dynamic. Skips type validation at runtime; the static
    /// checker treats it as compatible with everything.
    Any,
    /// Optional sugar for `T | nil`.
    Optional(&'static Ty),
    /// Generic application: `List<T>` is `Apply("list", &[T])`,
    /// `Result<T, E>` is `Apply("Result", &[T, E])`, `Schema<T>` is
    /// [`Ty::SchemaOf`].
    Apply(&'static str, &'static [Ty]),
    /// Union of N alternatives. Empty unions are rejected by the
    /// parser-side converter.
    Union(&'static [Ty]),
    /// Function type. Stores params and return as references so the literal
    /// stays `Copy`.
    Fn(&'static [Ty], &'static Ty),
    /// Record/shape type with named fields.
    Shape(&'static [ShapeFieldDescriptor]),
    /// `Schema<T>` marker — semantically `Apply("Schema", &[Generic(T)])`
    /// but distinguished so the type checker can pull the bound `T` from
    /// the *value* of the schema arg (not its declared type).
    SchemaOf(&'static str),
    /// Bottom type (no return).
    Never,
    /// Integer literal type: `0`, `1`. Assignable to `int`.
    LitInt(i64),
    /// String literal type: `"pass"`. Assignable to `string`.
    LitString(&'static str),
}

#[derive(Debug, Clone, Copy)]
pub struct ShapeFieldDescriptor {
    pub name: &'static str,
    pub ty: Ty,
    pub optional: bool,
}

impl ShapeFieldDescriptor {
    pub const fn new(name: &'static str, ty: Ty) -> Self {
        Self {
            name,
            ty,
            optional: false,
        }
    }

    pub const fn optional(name: &'static str, ty: Ty) -> Self {
        Self {
            name,
            ty,
            optional: true,
        }
    }
}

impl Ty {
    /// True when this type carries no constraints (validation is a no-op).
    pub const fn is_any(&self) -> bool {
        matches!(self, Ty::Any)
    }
}

impl BuiltinSignature {
    /// Non-generic, fixed-arity builtin: no type parameters, no rest, no
    /// where-clause bounds. Covers ~70% of the registry; lets each call
    /// site stay on a single logical line.
    pub const fn simple(name: &'static str, params: &'static [Param], returns: Ty) -> Self {
        Self {
            name,
            params,
            returns,
            type_params: &[],
            has_rest: false,
            where_clauses: &[],
        }
    }

    /// Non-generic builtin whose final parameter is variadic (rest).
    /// Equivalent to [`Self::simple`] with `has_rest: true`.
    pub const fn variadic(name: &'static str, params: &'static [Param], returns: Ty) -> Self {
        Self {
            name,
            params,
            returns,
            type_params: &[],
            has_rest: true,
            where_clauses: &[],
        }
    }

    /// Generic, fixed-arity builtin: declares type parameters, no rest,
    /// no where-clause bounds. Use the struct literal directly when both
    /// generics and where-clauses or rest are needed.
    pub const fn generic(
        name: &'static str,
        type_params: &'static [&'static str],
        params: &'static [Param],
        returns: Ty,
    ) -> Self {
        Self {
            name,
            params,
            returns,
            type_params,
            has_rest: false,
            where_clauses: &[],
        }
    }

    /// Number of required parameters (those without defaults).
    pub fn required_params(&self) -> usize {
        self.params.iter().filter(|p| !p.optional).count()
    }

    /// True when this builtin recognises `name` as one of its declared
    /// generic type parameters.
    pub fn is_type_param(&self, name: &str) -> bool {
        self.type_params.contains(&name)
    }

    /// True when this builtin declares any generic type parameters.
    pub fn is_generic(&self) -> bool {
        !self.type_params.is_empty()
    }

    /// Materialize the type parameter names as owned strings (for use in
    /// the type checker's existing scope/binding APIs which key off
    /// `Vec<String>`).
    pub fn type_param_names(&self) -> Vec<String> {
        self.type_params.iter().map(|s| (*s).to_string()).collect()
    }

    /// Where-clause constraints as `(type_param, interface)` strings.
    pub fn where_clause_strings(&self) -> Vec<(String, String)> {
        self.where_clauses
            .iter()
            .map(|(tp, iface)| ((*tp).to_string(), (*iface).to_string()))
            .collect()
    }
}

/// Public view of one builtin used by `harn-lint` and other crates that need
/// just identifier + return-type hints (no parameter types).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinMetadata {
    pub name: &'static str,
    pub return_types: &'static [&'static str],
}

// ---- Convenience constants ----
//
// Used pervasively in builtin signature literals to keep individual entries
// terse. Add new constants here when a type appears repeatedly enough to
// warrant a shorthand (avoid one-off shorthands).

pub const TY_ANY: Ty = Ty::Any;
pub const TY_BOOL: Ty = Ty::Named("bool");
pub const TY_BYTES: Ty = Ty::Named("bytes");
pub const TY_CLOSURE: Ty = Ty::Named("closure");
pub const TY_DICT: Ty = Ty::Named("dict");
pub const TY_DURATION: Ty = Ty::Named("duration");
pub const TY_FLOAT: Ty = Ty::Named("float");
pub const TY_INT: Ty = Ty::Named("int");
pub const TY_LIST: Ty = Ty::Named("list");
pub const TY_NEVER: Ty = Ty::Never;
pub const TY_NIL: Ty = Ty::Named("nil");
pub const TY_STRING: Ty = Ty::Named("string");

/// `string | nil`.
pub const TY_STRING_OR_NIL: Ty = Ty::Union(&[TY_STRING, TY_NIL]);
/// `int | nil`.
pub const TY_INT_OR_NIL: Ty = Ty::Union(&[TY_INT, TY_NIL]);
/// `dict | nil`.
pub const TY_DICT_OR_NIL: Ty = Ty::Union(&[TY_DICT, TY_NIL]);
/// `bytes | nil`.
pub const TY_BYTES_OR_NIL: Ty = Ty::Union(&[TY_BYTES, TY_NIL]);
/// `int | float`.
pub const TY_NUMBER: Ty = Ty::Union(&[TY_INT, TY_FLOAT]);
