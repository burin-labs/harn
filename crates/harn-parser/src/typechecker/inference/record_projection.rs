//! Key-dependent record results and their call-site validation.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;

use harn_builtin_meta::{CapabilityId, RecordProjection};

use crate::ast::{Node, SNode, ShapeField, TypeExpr};
use crate::diagnostic_codes::Code;

use super::super::{format::format_type, scope::TypeScope, union::simplify_union, TypeChecker};

/// A literal list guarantees its singleton keys. A runtime list only bounds
/// possible keys, even when its element type is a finite string union.
#[derive(Default)]
struct SelectedKeys {
    possible: Option<BTreeSet<String>>,
    guaranteed: BTreeSet<String>,
}

impl SelectedKeys {
    fn merge(&mut self, other: Self) {
        self.possible = match (self.possible.take(), other.possible) {
            (Some(mut left), Some(right)) => {
                left.extend(right);
                Some(left)
            }
            _ => None,
        };
        self.guaranteed.extend(other.guaranteed);
    }
}

struct RecordFields {
    fields: Vec<ShapeField>,
    tail: Option<TypeExpr>,
}

impl TypeChecker {
    /// Whether `value` is a `pick` call or a `const` alias of one. Such a
    /// binding takes the strict field-access path without a written annotation.
    pub(in crate::typechecker) fn has_projection_contract(
        &self,
        value: &SNode,
        scope: &TypeScope,
    ) -> bool {
        match &value.node {
            Node::Identifier(name) => scope
                .get_flow_alias(name)
                .is_some_and(|alias| self.has_projection_contract(alias, scope)),
            Node::FunctionCall { name, .. } => {
                scope.get_var_before_fn(name).is_none()
                    && scope.get_fn(name).is_none()
                    && !self.name_is_imported(name)
                    && self
                        .lookup_builtin(name)
                        .is_some_and(|sig| sig.projection.is_some())
            }
            Node::BinaryOp { op, right, .. } if op == "|>" => {
                self.has_projection_contract(right, scope)
            }
            _ => false,
        }
    }

    /// The result type of a builtin call whose shape follows from its
    /// arguments: a builtin that returns its first argument's type, or a
    /// key-dependent projection. `Break(decided)` means this branch owns the
    /// answer, including a `None` for a projection it cannot type.
    pub(super) fn infer_builtin_shape_call(
        &self,
        name: &str,
        args: &[SNode],
        scope: &TypeScope,
    ) -> ControlFlow<Option<TypeExpr>> {
        if Self::builtin_preserves_first_arg_type(name) {
            if let Some(first_type) = args.first().and_then(|arg| self.infer_type(arg, scope)) {
                return ControlFlow::Break(Some(first_type));
            }
        }
        if self.name_is_imported(name) || args.iter().any(|arg| matches!(arg.node, Node::Spread(_)))
        {
            return ControlFlow::Continue(());
        }
        match self.lookup_builtin(name).and_then(|sig| sig.projection) {
            Some(projection) => {
                ControlFlow::Break(self.infer_record_projection(projection, args, scope))
            }
            None => ControlFlow::Continue(()),
        }
    }

    pub(super) fn infer_record_projection(
        &self,
        projection: RecordProjection,
        args: &[SNode],
        scope: &TypeScope,
    ) -> Option<TypeExpr> {
        let RecordProjection::Pick { source, keys } = projection;
        let source = self.infer_type(args.get(source)?, scope)?;
        let keys = self.selected_keys(args.get(keys)?, scope);
        self.project_record_type(&source, &keys, scope).ok()
    }

    pub(super) fn check_record_projection(
        &mut self,
        name: &str,
        projection: RecordProjection,
        args: &[SNode],
        scope: &TypeScope,
    ) {
        let RecordProjection::Pick { source, keys } = projection;
        let (Some(source_node), Some(keys_node)) = (args.get(source), args.get(keys)) else {
            return;
        };
        let Some(source_type) = self.infer_type(source_node, scope) else {
            return;
        };
        let keys = self.selected_keys(keys_node, scope);
        if let Err(reason) = self.project_record_type(&source_type, &keys, scope) {
            self.error_at(
                Code::ArgumentTypeMismatch,
                format!("{name}: {reason}"),
                keys_node.span,
            );
        }
    }

    fn selected_keys(&self, node: &SNode, scope: &TypeScope) -> SelectedKeys {
        let literal = Self::projection_key_literal(node, scope);
        let node = literal
            .filter(|node| matches!(node.node, Node::ListLiteral(_)))
            .unwrap_or(node);
        if let Node::ListLiteral(items) = &node.node {
            let mut selected = SelectedKeys {
                possible: Some(BTreeSet::new()),
                ..SelectedKeys::default()
            };
            for item in items {
                let keys = match &item.node {
                    Node::Spread(value) => self.selected_keys(value, scope),
                    Node::StringLiteral(key) | Node::RawStringLiteral(key) => {
                        Self::singleton_key(key.clone())
                    }
                    _ => match Self::projection_key_literal(item, scope).map(|node| &node.node) {
                        Some(Node::StringLiteral(key) | Node::RawStringLiteral(key)) => {
                            Self::singleton_key(key.clone())
                        }
                        _ => self
                            .infer_type(item, scope)
                            .map(|ty| self.key_element(&ty, scope))
                            .unwrap_or_default(),
                    },
                };
                selected.merge(keys);
            }
            return selected;
        }
        match self
            .infer_type(node, scope)
            .map(|ty| self.resolve_alias(&ty, scope))
        {
            Some(TypeExpr::Tuple(items)) => {
                let mut selected = SelectedKeys {
                    possible: Some(BTreeSet::new()),
                    ..SelectedKeys::default()
                };
                for item in items {
                    selected.merge(self.key_element(&item, scope));
                }
                selected
            }
            Some(TypeExpr::List(element)) => SelectedKeys {
                possible: self.key_element(&element, scope).possible,
                guaranteed: BTreeSet::new(),
            },
            _ => SelectedKeys::default(),
        }
    }

    fn projection_key_literal<'a>(node: &'a SNode, scope: &'a TypeScope) -> Option<&'a SNode> {
        let Node::Identifier(name) = &node.node else {
            return Some(node);
        };
        scope.get_flow_alias(name).filter(|value| {
            matches!(
                value.node,
                Node::StringLiteral(_) | Node::RawStringLiteral(_) | Node::ListLiteral(_)
            )
        })
    }

    fn singleton_key(key: String) -> SelectedKeys {
        let keys = BTreeSet::from([key]);
        SelectedKeys {
            possible: Some(keys.clone()),
            guaranteed: keys,
        }
    }

    fn key_element(&self, ty: &TypeExpr, scope: &TypeScope) -> SelectedKeys {
        match self.resolve_alias(ty, scope) {
            TypeExpr::LitString(key) => Self::singleton_key(key),
            TypeExpr::Union(members) => {
                let mut possible = BTreeSet::new();
                for member in members {
                    let Some(keys) = self.key_element(&member, scope).possible else {
                        return SelectedKeys::default();
                    };
                    possible.extend(keys);
                }
                let guaranteed = if possible.len() == 1 {
                    possible.clone()
                } else {
                    BTreeSet::new()
                };
                SelectedKeys {
                    possible: Some(possible),
                    guaranteed,
                }
            }
            _ => SelectedKeys::default(),
        }
    }

    fn project_record_type(
        &self,
        ty: &TypeExpr,
        keys: &SelectedKeys,
        scope: &TypeScope,
    ) -> Result<TypeExpr, String> {
        let resolved = self.resolve_alias(ty, scope);
        if let TypeExpr::Union(members) = &resolved {
            let projected = members
                .iter()
                .map(|member| self.project_record_type(member, keys, scope))
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(simplify_union(projected));
        }
        let RecordFields { fields, tail } = self.projection_fields(&resolved, scope)?;
        let mut projected = Vec::new();
        for field in &fields {
            if keys
                .possible
                .as_ref()
                .is_none_or(|keys| keys.contains(&field.name))
            {
                projected.push(ShapeField::synthetic(
                    field.name.clone(),
                    field.type_expr.clone(),
                    field.optional || !keys.guaranteed.contains(&field.name),
                ));
            }
        }
        if let Some(possible) = &keys.possible {
            for key in possible {
                if fields.iter().any(|field| &field.name == key) {
                    continue;
                }
                let Some(value_type) = &tail else {
                    return Err(format!(
                        "unknown field `{key}` in `{}`",
                        format_type(&resolved)
                    ));
                };
                projected.push(ShapeField::synthetic(key, value_type.clone(), true));
            }
            return Ok(TypeExpr::Shape(projected));
        }
        Ok(match tail {
            Some(value) if projected.is_empty() => Self::projection_map(value),
            Some(value) => TypeExpr::OpenShape {
                fields: projected,
                rests: vec![Self::projection_map(value)],
            },
            None => TypeExpr::Shape(projected),
        })
    }

    fn projection_map(value: TypeExpr) -> TypeExpr {
        TypeExpr::DictType(Box::new(TypeExpr::Named("string".into())), Box::new(value))
    }

    fn projection_fields(&self, ty: &TypeExpr, scope: &TypeScope) -> Result<RecordFields, String> {
        let unknown = || TypeExpr::Named("unknown".into());
        let open = |value| RecordFields {
            fields: Vec::new(),
            tail: Some(value),
        };
        match self.resolve_alias(ty, scope) {
            TypeExpr::Shape(fields) => Ok(RecordFields { fields, tail: None }),
            TypeExpr::OpenShape { fields, rests } => {
                let mut tails = Vec::new();
                for rest in rests {
                    let rest = self.projection_fields(&rest, scope)?;
                    if let Some(tail) = rest.tail {
                        tails.push(tail);
                    }
                }
                Ok(RecordFields {
                    fields,
                    tail: (!tails.is_empty()).then(|| simplify_union(tails)),
                })
            }
            TypeExpr::DictType(_, value) => Ok(open(*value)),
            TypeExpr::Named(name) if name == "Harness" => Ok(RecordFields {
                fields: CapabilityId::ALL
                    .iter()
                    .map(|cap| {
                        ShapeField::synthetic(
                            cap.field_name(),
                            TypeExpr::Named(cap.type_name().into()),
                            false,
                        )
                    })
                    .collect(),
                tail: None,
            }),
            TypeExpr::Named(name) if matches!(name.as_str(), "any" | "_" | "dict") => {
                Ok(open(unknown()))
            }
            TypeExpr::Named(name) if scope.is_generic_type_param(&name) => Ok(open(unknown())),
            TypeExpr::Named(name) if scope.get_struct(&name).is_some() => {
                self.project_struct_fields(&name, &[], scope)
            }
            TypeExpr::Applied { name, args } if scope.get_struct(&name).is_some() => {
                self.project_struct_fields(&name, &args, scope)
            }
            TypeExpr::Intersection(members) => {
                let mut fields = BTreeMap::<String, ShapeField>::new();
                let mut tails = Vec::new();
                for member in members {
                    let record = self.projection_fields(&member, scope)?;
                    for field in record.fields {
                        fields
                            .entry(field.name.clone())
                            .and_modify(|existing| {
                                existing.optional &= field.optional;
                                if existing.type_expr != field.type_expr {
                                    existing.type_expr = TypeExpr::Intersection(vec![
                                        existing.type_expr.clone(),
                                        field.type_expr.clone(),
                                    ]);
                                }
                            })
                            .or_insert(field);
                    }
                    if let Some(tail) = record.tail {
                        tails.push(tail);
                    }
                }
                Ok(RecordFields {
                    fields: fields.into_values().collect(),
                    tail: (!tails.is_empty()).then(|| simplify_union(tails)),
                })
            }
            other => Err(format!(
                "expected a record, dictionary, or Harness; found `{}`",
                format_type(&other)
            )),
        }
    }

    fn project_struct_fields(
        &self,
        name: &str,
        args: &[TypeExpr],
        scope: &TypeScope,
    ) -> Result<RecordFields, String> {
        let info = scope
            .get_struct(name)
            .expect("struct resolved by projection_fields");
        let bindings = info
            .type_params
            .iter()
            .map(|param| param.name.clone())
            .zip(args.iter().cloned())
            .collect();
        Ok(RecordFields {
            fields: info
                .fields
                .iter()
                .map(|field| {
                    ShapeField::synthetic(
                        &field.name,
                        field
                            .type_expr
                            .as_ref()
                            .map(|ty| Self::apply_type_bindings(ty, &bindings))
                            .unwrap_or_else(|| TypeExpr::Named("unknown".into())),
                        field.optional,
                    )
                })
                .collect(),
            tail: None,
        })
    }
}
