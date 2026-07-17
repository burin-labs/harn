//! Re-exports of builtin signature shape types from `harn-builtin-meta`, plus
//! parser-local conversion helpers from the const IR (`Ty`) to the runtime
//! IR (`TypeExpr`).
//!
//! The const-constructible types live in `harn-builtin-meta` (a dep-free
//! crate consumed by both the parser and `harn-vm`). Conversion to the
//! parser's owned `TypeExpr` requires types that are private to this crate,
//! so it lives here as an extension trait.

use crate::ast::{ShapeField, TypeExpr};

pub use harn_builtin_meta::{
    BuiltinMetadata, BuiltinSignature, Param, ShapeFieldDescriptor, Ty, TY_ANY, TY_BOOL, TY_BYTES,
    TY_BYTES_OR_NIL, TY_CLOSURE, TY_DECIMAL, TY_DICT, TY_DICT_OR_NIL, TY_DURATION, TY_FLOAT,
    TY_INT, TY_INT_OR_NIL, TY_LIST, TY_NEVER, TY_NIL, TY_NUMBER, TY_STRING, TY_STRING_OR_NIL,
};

/// Convert a const-IR [`Ty`] into the parser's owned [`TypeExpr`]. Generic
/// references stay as `Named(name)` so the checker's existing scope-based
/// generic-param resolution applies.
pub fn ty_to_type_expr(ty: &Ty) -> TypeExpr {
    match ty {
        Ty::Named(name) => TypeExpr::Named((*name).into()),
        Ty::Generic(name) => TypeExpr::Named((*name).into()),
        Ty::Any => TypeExpr::Named("any".into()),
        Ty::Optional(inner) => {
            TypeExpr::Union(vec![ty_to_type_expr(inner), TypeExpr::Named("nil".into())])
        }
        Ty::Apply(name, args) => TypeExpr::Applied {
            name: (*name).into(),
            args: args.iter().map(ty_to_type_expr).collect(),
        },
        Ty::Union(members) => TypeExpr::Union(members.iter().map(ty_to_type_expr).collect()),
        Ty::Fn(params, return_type) => TypeExpr::FnType {
            params: params.iter().map(ty_to_type_expr).collect(),
            return_type: Box::new(ty_to_type_expr(return_type)),
        },
        Ty::Shape(fields) => TypeExpr::Shape(
            fields
                .iter()
                .map(|f| ShapeField::synthetic(f.name, ty_to_type_expr(&f.ty), f.optional))
                .collect(),
        ),
        Ty::SchemaOf(name) => TypeExpr::Applied {
            name: "Schema".into(),
            args: vec![TypeExpr::Named((*name).into())],
        },
        Ty::Never => TypeExpr::Never,
        Ty::LitInt(v) => TypeExpr::LitInt(*v),
        Ty::LitString(s) => TypeExpr::LitString((*s).into()),
    }
}

/// Parser-side extension methods on [`Ty`] and [`BuiltinSignature`] that
/// depend on the parser's owned AST types (kept out of `harn-builtin-meta`
/// so that crate stays dep-free).
pub trait TyExt {
    /// Materialize as a runtime [`TypeExpr`].
    fn to_type_expr(&self) -> TypeExpr;
}

impl TyExt for Ty {
    fn to_type_expr(&self) -> TypeExpr {
        ty_to_type_expr(self)
    }
}

pub trait BuiltinSignatureExt {
    /// Materialize per-parameter types as owned [`TypeExpr`]s for the type
    /// checker's call-site validation.
    fn param_type_exprs(&self) -> Vec<TypeExpr>;
    /// Owned [`TypeExpr`] return type.
    fn return_type_expr(&self) -> TypeExpr;
}

impl BuiltinSignatureExt for BuiltinSignature {
    fn param_type_exprs(&self) -> Vec<TypeExpr> {
        self.params.iter().map(|p| ty_to_type_expr(&p.ty)).collect()
    }

    fn return_type_expr(&self) -> TypeExpr {
        ty_to_type_expr(&self.returns)
    }
}
