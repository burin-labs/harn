//! JSONB helpers backed by Postgres operators/functions.
//!
//! These helpers bind both JSON operands and JSONPath strings instead of
//! building SQL fragments in Harn source. They are intentionally small:
//! callers that need table-column predicates can still use `pg_query`
//! directly, while scripts that already have JSON values can delegate the
//! exact Postgres semantics to the database.

use std::sync::Arc;

use crate::stdlib::macros::{harn_builtin, BuiltinSignature, Param, TY_ANY, TY_BOOL, TY_LIST};
use crate::value::{VmError, VmValue};

use super::{query_rows, required_arg, runtime_error, QueryRouting};

const JSONB_PATH_SQL: &str = "SELECT jsonb_path_query($1::jsonb, $2::jsonpath) AS value";
const JSONB_MERGE_SQL: &str = "SELECT ($1::jsonb || $2::jsonb) AS value";
const JSONB_CONTAINS_SQL: &str = "SELECT ($1::jsonb @> $2::jsonb) AS contains";

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg.jsonb.path", &[Param::new("args", TY_ANY)], TY_LIST),
    kind = "async",
    category = "postgres"
)]
async fn pg_jsonb_path_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let target = required_arg(&args, 0, "pg.jsonb.path", "pool or transaction handle")?;
    let document = required_arg(&args, 1, "pg.jsonb.path", "document")?;
    let path = required_string(&args, 2, "pg.jsonb.path", "jsonpath")?;
    let params = [
        document.clone(),
        VmValue::String(arcstr::ArcStr::from(path.to_string())),
    ];
    let rows = query_rows(target, JSONB_PATH_SQL, &params, QueryRouting::Primary).await?;
    rows.into_iter()
        .map(|row| extract_column(row, "value", "pg.jsonb.path"))
        .collect::<Result<Vec<_>, _>>()
        .map(|values| VmValue::List(Arc::new(values)))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg.jsonb.merge", &[Param::new("args", TY_ANY)], TY_ANY),
    kind = "async",
    category = "postgres"
)]
async fn pg_jsonb_merge_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let target = required_arg(&args, 0, "pg.jsonb.merge", "pool or transaction handle")?;
    let left = required_arg(&args, 1, "pg.jsonb.merge", "left document")?;
    let right = required_arg(&args, 2, "pg.jsonb.merge", "right document")?;
    let rows = query_rows(
        target,
        JSONB_MERGE_SQL,
        &[left.clone(), right.clone()],
        QueryRouting::Primary,
    )
    .await?;
    first_column(rows, "value", "pg.jsonb.merge")
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg.jsonb.contains", &[Param::new("args", TY_ANY)], TY_BOOL),
    kind = "async",
    category = "postgres"
)]
async fn pg_jsonb_contains_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let target = required_arg(&args, 0, "pg.jsonb.contains", "pool or transaction handle")?;
    let left = required_arg(&args, 1, "pg.jsonb.contains", "left document")?;
    let right = required_arg(&args, 2, "pg.jsonb.contains", "right document")?;
    let rows = query_rows(
        target,
        JSONB_CONTAINS_SQL,
        &[left.clone(), right.clone()],
        QueryRouting::Primary,
    )
    .await?;
    match first_column(rows, "contains", "pg.jsonb.contains")? {
        VmValue::Bool(value) => Ok(VmValue::Bool(value)),
        _ => Err(runtime_error(
            "pg.jsonb.contains: database returned a non-bool result",
        )),
    }
}

fn required_string<'a>(
    args: &'a [VmValue],
    index: usize,
    builtin: &'static str,
    label: &'static str,
) -> Result<&'a str, VmError> {
    match args.get(index) {
        Some(VmValue::String(text)) if !text.trim().is_empty() => Ok(text.as_ref()),
        _ => Err(runtime_error(format!("{builtin}: {label} is required"))),
    }
}

fn first_column(
    rows: Vec<VmValue>,
    column: &'static str,
    builtin: &'static str,
) -> Result<VmValue, VmError> {
    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| runtime_error(format!("{builtin}: database returned no rows")))?;
    extract_column(row, column, builtin)
}

fn extract_column(
    row: VmValue,
    column: &'static str,
    builtin: &'static str,
) -> Result<VmValue, VmError> {
    row.as_dict()
        .and_then(|dict| dict.get(column))
        .cloned()
        .ok_or_else(|| runtime_error(format!("{builtin}: database result missing `{column}`")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_jsonpath_must_be_non_empty_string() {
        assert!(required_string(
            &[VmValue::String(arcstr::ArcStr::from("$.items"))],
            0,
            "pg",
            "path"
        )
        .is_ok());
        assert!(required_string(
            &[VmValue::String(arcstr::ArcStr::from(""))],
            0,
            "pg",
            "path"
        )
        .is_err());
        assert!(required_string(&[VmValue::Int(1)], 0, "pg", "path").is_err());
    }
}
