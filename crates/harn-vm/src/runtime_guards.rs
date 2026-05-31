use std::sync::Arc;

use harn_parser::TypeExpr;

use crate::schema::CanonicalParamSchema;

#[derive(Debug, Clone)]
pub(crate) enum RuntimeParamGuard {
    CanonicalSchema(CanonicalParamSchema),
    InvalidSchema(Arc<str>),
    TypeExpr(TypeExpr),
}

impl RuntimeParamGuard {
    pub(crate) fn from_type_expr(type_expr: &TypeExpr) -> Self {
        if let Some(schema) = crate::compiler::Compiler::type_expr_to_schema_value(type_expr) {
            return match crate::schema::canonical_param_schema(&schema) {
                Ok(schema) => Self::CanonicalSchema(schema),
                Err(error) => Self::InvalidSchema(Arc::from(error)),
            };
        }

        Self::TypeExpr(type_expr.clone())
    }
}
