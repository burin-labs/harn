//! Project host records from their owning Harn declarations.

use super::{
    records::{Field, FieldKind, Integer, Record},
    support::ProtocolArtifactSource,
};
use harn_parser::{parse_source, Node, ShapeField, TypeExpr};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn load(
    source: &ProtocolArtifactSource,
    modules: &[&str],
    names: &[&str],
) -> Result<Vec<Record>, String> {
    let mut declarations = BTreeMap::new();
    for module in modules {
        let path = format!("crates/harn-stdlib/src/stdlib/{module}.harn");
        for node in parse_source(&source.read_text(&path)?).map_err(|error| error.to_string())? {
            if let Node::TypeDecl {
                name, type_expr, ..
            } = node.node
            {
                if declarations.insert(name.clone(), type_expr).is_some() {
                    return Err(format!("duplicate protocol type {name}"));
                }
            }
        }
    }
    let mut records = Vec::new();
    for name in names {
        let Some(TypeExpr::Shape(fields)) = declarations.get(*name) else {
            return Err(format!("protocol projection requires record {name}"));
        };
        let record = record(&format!("Harn{name}"), fields, &mut records)?;
        records.push(record);
    }
    Ok(records)
}

fn record(name: &str, fields: &[ShapeField], records: &mut Vec<Record>) -> Result<Record, String> {
    let mut names = BTreeSet::new();
    let fields = fields
        .iter()
        .map(|field| {
            if !names.insert(&field.name) {
                return Err(format!("duplicate protocol field {name}.{}", field.name));
            }
            let mut kind = field_kind(&field.type_expr, name, &field.name, records)?;
            if name.ends_with("ActivityRecord") && field.name == "kind" {
                if let FieldKind::Literal { wire_type, .. } = &mut kind {
                    *wire_type = Some("HarnActivityKind".into());
                }
            }
            // Target metadata preserves the existing host API where Harn's
            // broader primitive type does not express that projection detail.
            kind = match (name, field.name.as_str()) {
                ("HarnToolPermissionScope", "tool_kind") => {
                    FieldKind::Named("HarnACPToolKind".into())
                }
                ("HarnToolPermissionScope", "side_effect") => {
                    FieldKind::Named("HarnSideEffectLevel".into())
                }
                ("HarnToolPermissionGrantEvidence", "reusable") => FieldKind::LiteralBool(false),
                ("HarnToolPermissionActivityRecord", "occurred_at_ms") => {
                    FieldKind::Integer(Integer::UnsignedHarn)
                }
                ("HarnConnectorSetupEvent", "sequence") => FieldKind::Integer(Integer::U64),
                _ => kind,
            };
            Ok(Field {
                wire_name: field.name.clone().into(),
                rust_name: field.name.clone().into(),
                kind,
                required: !field.optional,
                identity: false,
            })
        })
        .collect::<Result<_, String>>()?;
    Ok(Record {
        name: name.into(),
        fields,
    })
}

fn field_kind(
    value: &TypeExpr,
    owner: &str,
    field: &str,
    records: &mut Vec<Record>,
) -> Result<FieldKind, String> {
    Ok(match value {
        TypeExpr::Named(name) => match name.as_str() {
            "string" => FieldKind::String,
            "int" => FieldKind::Integer(Integer::Harn),
            "bool" => FieldKind::Bool,
            name if ["ExternalAction", "ToolPermission", "ConnectorSetup"]
                .iter()
                .any(|prefix| name.starts_with(prefix)) =>
            {
                FieldKind::Named(format!("Harn{name}"))
            }
            _ => return Err(format!("unsupported {owner}.{field} type {value:?}")),
        },
        TypeExpr::LitString(value) => FieldKind::Literal {
            value: value.clone(),
            wire_type: None,
        },
        TypeExpr::List(inner) => {
            FieldKind::List(Box::new(field_kind(inner, owner, field, records)?))
        }
        TypeExpr::Shape(fields) => {
            let mut chars = field.chars();
            let suffix = chars
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_default()
                + chars.as_str();
            let name = format!("{owner}{suffix}");
            let nested = record(&name, fields, records)?;
            records.push(nested);
            FieldKind::Named(name)
        }
        _ => return Err(format!("unsupported {owner}.{field} type {value:?}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_source_fields_fail_before_emitting_host_members() {
        let source = "type Activity = {kind: \"external_action\", kind: \"external_action\"}";
        let program = parse_source(source).expect("valid Harn declaration");
        let Node::TypeDecl {
            type_expr: TypeExpr::Shape(fields),
            ..
        } = &program[0].node
        else {
            panic!("record declaration was not reached");
        };
        assert_eq!(
            record("HarnActivity", fields, &mut Vec::new()).unwrap_err(),
            "duplicate protocol field HarnActivity.kind"
        );
    }
}
