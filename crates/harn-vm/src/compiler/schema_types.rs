use std::collections::{BTreeMap, HashSet};

use harn_parser::{substitute_type_expr, ShapeField, TypeExpr};

use crate::chunk::Op;
use crate::value::VmValue;

use super::Compiler;

#[derive(Clone)]
pub(super) enum SchemaFragment {
    Value(VmValue),
    Ref(String),
    Dict(BTreeMap<String, SchemaFragment>),
    List(Vec<SchemaFragment>),
}

impl Compiler {
    pub(super) fn schema_fragment_for_alias(&self, name: &str) -> Option<SchemaFragment> {
        let alias = self.type_aliases.get(name)?;
        if !alias.type_params.is_empty() {
            return None;
        }
        match alias.body.as_ref() {
            Some(body) => self.schema_fragment_for_type(body, &mut HashSet::new()),
            None => Some(SchemaFragment::Ref(name.to_string())),
        }
    }

    fn schema_fragment_for_type(
        &self,
        ty: &TypeExpr,
        visiting: &mut HashSet<String>,
    ) -> Option<SchemaFragment> {
        if let Some(value) = Self::type_expr_to_schema_value(ty) {
            return Some(SchemaFragment::Value(value));
        }
        match ty {
            TypeExpr::Named(name) => {
                let alias = self.type_aliases.get(name)?;
                let Some(body) = alias.body.as_ref() else {
                    return Some(SchemaFragment::Ref(name.clone()));
                };
                if !alias.type_params.is_empty() || !visiting.insert(name.clone()) {
                    return None;
                }
                let fragment = self.schema_fragment_for_type(body, visiting);
                visiting.remove(name);
                fragment
            }
            TypeExpr::Shape(fields) => self.schema_fragment_for_shape(fields, None, visiting),
            TypeExpr::OpenShape { fields, rests } => {
                let mut additional = None;
                let mut row_fragments = Vec::new();
                for rest in rests {
                    if let TypeExpr::DictType(key, value) = rest {
                        if matches!(key.as_ref(), TypeExpr::Named(name) if name == "string") {
                            additional = Some(value.as_ref());
                            continue;
                        }
                    }
                    // A free row variable (`{..., ...rest}`) or a bare `dict`
                    // tail has no concrete schema to merge — it only marks the
                    // record open, so unknown fields of any type validate. Skip
                    // it rather than failing to lower the whole alias. Bound
                    // alias tails (`...SomeShape`) still merge as an `all_of`
                    // branch below.
                    if let TypeExpr::Named(name) = rest {
                        let is_bound_alias = self
                            .type_aliases
                            .get(name)
                            .is_some_and(|alias| alias.body.is_some());
                        if !is_bound_alias {
                            continue;
                        }
                    }
                    row_fragments.push(self.schema_fragment_for_type(rest, visiting)?);
                }
                let base = self.schema_fragment_for_shape(fields, additional, visiting)?;
                if row_fragments.is_empty() {
                    Some(base)
                } else {
                    let mut branches = Vec::with_capacity(row_fragments.len() + 1);
                    branches.push(base);
                    branches.extend(row_fragments);
                    Some(SchemaFragment::Dict(BTreeMap::from([(
                        "all_of".to_string(),
                        SchemaFragment::List(branches),
                    )])))
                }
            }
            TypeExpr::List(inner) => Some(SchemaFragment::Dict(BTreeMap::from([
                (
                    "type".to_string(),
                    SchemaFragment::Value(VmValue::String(arcstr::ArcStr::from("list"))),
                ),
                (
                    "items".to_string(),
                    self.schema_fragment_for_type(inner, visiting)?,
                ),
            ]))),
            TypeExpr::DictType(key, value) if matches!(key.as_ref(), TypeExpr::Named(name) if name == "string") => {
                Some(SchemaFragment::Dict(BTreeMap::from([
                    (
                        "type".to_string(),
                        SchemaFragment::Value(VmValue::String(arcstr::ArcStr::from("dict"))),
                    ),
                    (
                        "additional_properties".to_string(),
                        self.schema_fragment_for_type(value, visiting)?,
                    ),
                ])))
            }
            TypeExpr::Union(members) | TypeExpr::Intersection(members) => {
                let key = if matches!(ty, TypeExpr::Union(_)) {
                    "union"
                } else {
                    "all_of"
                };
                let branches = members
                    .iter()
                    .map(|member| self.schema_fragment_for_type(member, visiting))
                    .collect::<Option<Vec<_>>>()?;
                Some(SchemaFragment::Dict(BTreeMap::from([(
                    key.to_string(),
                    SchemaFragment::List(branches),
                )])))
            }
            TypeExpr::Applied { name, args } => {
                let alias = self.type_aliases.get(name)?;
                let body = alias.body.as_ref()?;
                if alias.type_params.len() != args.len() || !visiting.insert(name.clone()) {
                    return None;
                }
                let bindings = alias
                    .type_params
                    .iter()
                    .zip(args)
                    .map(|(param, arg)| (param.name.clone(), arg.clone()))
                    .collect();
                let instantiated = substitute_type_expr(body, &bindings);
                let fragment = self.schema_fragment_for_type(&instantiated, visiting);
                visiting.remove(name);
                fragment
            }
            TypeExpr::Owned(inner) => self.schema_fragment_for_type(inner, visiting),
            _ => None,
        }
    }

    fn schema_fragment_for_shape(
        &self,
        fields: &[ShapeField],
        additional: Option<&TypeExpr>,
        visiting: &mut HashSet<String>,
    ) -> Option<SchemaFragment> {
        let mut properties = BTreeMap::new();
        let mut required = Vec::new();
        for field in fields {
            let mut field_schema = self.schema_fragment_for_type(&field.type_expr, visiting)?;
            if field.optional {
                field_schema = SchemaFragment::Dict(BTreeMap::from([(
                    "union".to_string(),
                    SchemaFragment::List(vec![
                        field_schema,
                        SchemaFragment::Value(VmValue::dict(BTreeMap::from([(
                            "type".to_string(),
                            VmValue::String(arcstr::ArcStr::from("nil")),
                        )]))),
                    ]),
                )]));
            } else {
                required.push(SchemaFragment::Value(VmValue::String(
                    arcstr::ArcStr::from(field.name.as_str()),
                )));
            }
            properties.insert(field.name.clone(), field_schema);
        }
        let mut schema = BTreeMap::from([
            (
                "type".to_string(),
                SchemaFragment::Value(VmValue::String(arcstr::ArcStr::from("dict"))),
            ),
            ("properties".to_string(), SchemaFragment::Dict(properties)),
        ]);
        if !required.is_empty() {
            schema.insert("required".to_string(), SchemaFragment::List(required));
        }
        if let Some(additional) = additional {
            schema.insert(
                "additional_properties".to_string(),
                self.schema_fragment_for_type(additional, visiting)?,
            );
        }
        Some(SchemaFragment::Dict(schema))
    }

    pub(super) fn emit_schema_fragment(&mut self, fragment: &SchemaFragment) {
        match fragment {
            SchemaFragment::Value(value) => self.emit_vm_value_literal(value),
            SchemaFragment::Ref(name) => self.emit_get_binding(name),
            SchemaFragment::Dict(entries) => {
                for (key, value) in entries {
                    let key_idx = self.string_constant(key);
                    self.chunk.emit_u16(Op::Constant, key_idx, self.line);
                    self.emit_schema_fragment(value);
                }
                self.chunk
                    .emit_u16(Op::BuildDict, entries.len() as u16, self.line);
            }
            SchemaFragment::List(items) => {
                for item in items {
                    self.emit_schema_fragment(item);
                }
                self.chunk
                    .emit_u16(Op::BuildList, items.len() as u16, self.line);
            }
        }
    }

    pub(super) fn emit_schema_for_alias(&mut self, name: &str) -> bool {
        let Some(fragment) = self.schema_fragment_for_alias(name) else {
            return false;
        };
        self.emit_schema_fragment(&fragment);
        true
    }
}
