//! Render a parsed [`Schema`] into Harn `type` declarations.
//!
//! The SQL-to-Harn type mapping mirrors the runtime row decode in
//! `harn-vm`'s `postgres::column_value`: whatever shape a column's value takes
//! when a query returns it is the shape the generated record type promises.
//! `NUMERIC`, timestamps, `UUID`, and friends surface as strings there, so they
//! surface as `string` here.

use super::ddl::{Schema, SqlType, Table};
use crate::commands::scaffold_common::pascal_identifier_from_snake;

/// Render the full generated file: a stable header plus one `type` per table.
pub(crate) fn render(schema: &Schema, header: &str, type_suffix: &str) -> String {
    let mut out = String::new();
    out.push_str(header);

    let mut first = true;
    for (table_name, table) in schema.tables() {
        if !first {
            out.push('\n');
        }
        first = false;
        out.push_str(&render_table(table_name, table, type_suffix));
    }

    out
}

fn render_table(table_name: &str, table: &Table, type_suffix: &str) -> String {
    let type_name = format!("{}{type_suffix}", pascal_identifier_from_snake(table_name));
    let mut out = format!("type {type_name} = {{\n");
    for column in &table.columns {
        let harn_type = harn_type_for(&column.sql_type, column.not_null);
        out.push_str(&format!("  {}: {harn_type},\n", column.name));
    }
    out.push_str("}\n");
    out
}

/// Map a column's SQL type + nullability to a Harn type expression.
fn harn_type_for(sql_type: &SqlType, not_null: bool) -> String {
    let mut ty = base_harn_type(&sql_type.base);
    for _ in 0..sql_type.array_dims {
        ty = format!("list<{ty}>");
    }
    if not_null {
        ty
    } else {
        // Postfix `?` is the canonical optional form the linter prefers
        // (HARN-LNT-048) over the equivalent `T | nil`.
        format!("{ty}?")
    }
}

/// The Harn scalar type for a normalized SQL base type name. Unknown types
/// (user-defined enums, composites, …) fall back to `string`, matching the
/// runtime decode's textual fallback.
fn base_harn_type(base: &str) -> String {
    match base {
        "bool" | "boolean" => "bool",
        "smallint" | "int2" | "smallserial" | "serial2" | "integer" | "int" | "int4" | "serial"
        | "serial4" | "bigint" | "int8" | "bigserial" | "serial8" => "int",
        "real" | "float4" | "double precision" | "float8" | "double" => "float",
        "json" | "jsonb" => "any",
        "bytea" => "bytes",
        "hstore" => return "dict<string, string?>".to_string(),
        "point" => return "{x: float, y: float}".to_string(),
        // numeric/money, all text-ish, temporal, network, and bit types decode
        // to strings; so does anything we do not recognize.
        _ => "string",
    }
    .to_string()
}
