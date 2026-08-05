//! Match-expression inference and positional pattern binding.

use std::collections::BTreeMap;

use crate::ast::{MatchArm, Node, SNode, TypeExpr};

use super::super::scope::{InferredType, TypeScope};
use super::super::union::simplify_union;
use super::super::TypeChecker;

impl TypeChecker {
    pub(in crate::typechecker) fn tuple_element_type(elements: &[TypeExpr]) -> TypeExpr {
        if elements.is_empty() {
            TypeExpr::Never
        } else {
            simplify_union(elements.to_vec())
        }
    }

    fn constant_subscript_index(index: &SNode) -> Option<i64> {
        match &index.node {
            Node::IntLiteral(value) => Some(*value),
            Node::UnaryOp { op, operand } if op == "-" => match &operand.node {
                Node::IntLiteral(value) => value.checked_neg(),
                _ => None,
            },
            _ => None,
        }
    }

    pub(in crate::typechecker) fn tuple_position(
        index: &SNode,
        arity: usize,
    ) -> Option<Result<usize, i64>> {
        let index = Self::constant_subscript_index(index)?;
        let position = if index < 0 {
            (arity as i64).checked_add(index)
        } else {
            Some(index)
        };
        match position {
            Some(position) if position >= 0 && (position as usize) < arity => {
                Some(Ok(position as usize))
            }
            _ => Some(Err(index)),
        }
    }

    pub(super) fn infer_match_expr_type(
        &self,
        value: &SNode,
        arms: &[MatchArm],
        scope: &TypeScope,
    ) -> InferredType {
        let value_type = self.infer_type(value, scope);
        let mut arm_types = Vec::new();
        for arm in arms {
            let mut arm_scope = scope.child();
            self.define_match_pattern_bindings(&arm.pattern, value_type.as_ref(), &mut arm_scope);
            self.narrow_match_subject(value, &arm.pattern, &mut arm_scope);
            if let Some(arm_type) = self.infer_block_type(&arm.body, &arm_scope).into_inferred() {
                arm_types.push(arm_type);
            }
        }
        match (arms.is_empty(), arm_types.len()) {
            (true, _) => Some(TypeExpr::Never),
            (false, 0) => None,
            (false, 1) => arm_types.pop(),
            (false, _) => Some(simplify_union(arm_types)),
        }
    }

    pub(in crate::typechecker) fn define_match_pattern_bindings(
        &self,
        pattern: &SNode,
        value_type: Option<&TypeExpr>,
        scope: &mut TypeScope,
    ) {
        match &pattern.node {
            Node::Identifier(name) if name != "_" => {
                scope.define_var(name, value_type.cloned());
            }
            Node::ListLiteral(elements) => {
                let resolved = value_type.map(|ty| self.resolve_alias(ty, scope));
                let item_type = resolved.as_ref().and_then(|ty| match ty {
                    TypeExpr::List(inner) => Some(inner.as_ref().clone()),
                    TypeExpr::Tuple(items) => Some(Self::tuple_element_type(items)),
                    _ => None,
                });
                for (position, element) in elements.iter().enumerate() {
                    match &element.node {
                        Node::Identifier(name) if name != "_" => {
                            let binding_type = match &resolved {
                                Some(TypeExpr::Tuple(items)) => items.get(position).cloned(),
                                _ => item_type.clone(),
                            };
                            scope.define_var(name, binding_type);
                        }
                        Node::Spread(inner) => {
                            if let Node::Identifier(name) = &inner.node {
                                if name != "_" {
                                    let rest_type = Some(match &resolved {
                                        Some(TypeExpr::Tuple(items)) => TypeExpr::List(Box::new(
                                            Self::tuple_element_type(&items[position..]),
                                        )),
                                        _ => match &item_type {
                                            Some(item) => TypeExpr::List(Box::new(item.clone())),
                                            None => TypeExpr::Named("list".into()),
                                        },
                                    });
                                    scope.define_var(name, rest_type);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Node::DictLiteral(entries) => {
                for entry in entries {
                    let Some(key) = (match &entry.key.node {
                        Node::StringLiteral(key) | Node::Identifier(key) => Some(key.as_str()),
                        _ => None,
                    }) else {
                        continue;
                    };
                    let Node::Identifier(name) = &entry.value.node else {
                        continue;
                    };
                    if name == "_" {
                        continue;
                    }
                    let binding_type =
                        value_type.and_then(|ty| match self.resolve_alias(ty, scope) {
                            TypeExpr::Shape(fields) => fields
                                .into_iter()
                                .find(|field| field.name == key)
                                .map(|field| field.type_expr),
                            TypeExpr::DictType(_, value) => Some(*value),
                            _ => None,
                        });
                    scope.define_var(name, binding_type);
                }
            }
            Node::EnumConstruct {
                enum_name,
                variant,
                args,
            } => self.define_enum_pattern_bindings(enum_name, variant, args, value_type, scope),
            Node::MethodCall {
                object,
                method,
                args,
            } => {
                if let Node::Identifier(enum_name) = &object.node {
                    self.define_enum_pattern_bindings(enum_name, method, args, value_type, scope);
                }
            }
            Node::FunctionCall { name, args, .. } => {
                let catalog = scope.lexical_match_pattern_catalog();
                if let crate::lexical::BareVariantResolution::Unique(enum_name) =
                    catalog.resolve_bare_variant(name)
                {
                    self.define_enum_pattern_bindings(enum_name, name, args, value_type, scope);
                }
            }
            _ => {}
        }
    }

    pub(in crate::typechecker) fn define_enum_pattern_bindings(
        &self,
        enum_name: &str,
        variant: &str,
        args: &[SNode],
        value_type: Option<&TypeExpr>,
        scope: &mut TypeScope,
    ) {
        let Some(enum_info) = scope.get_enum(enum_name) else {
            return;
        };
        let Some(variant_info) = enum_info.variants.iter().find(|item| item.name == variant) else {
            return;
        };
        let unbound_param_names: std::collections::BTreeSet<String> = enum_info
            .type_params
            .iter()
            .map(|param| param.name.clone())
            .filter(|name| !scope.is_generic_type_param(name))
            .collect();
        let type_bindings: BTreeMap<String, TypeExpr> = value_type
            .map(|ty| self.resolve_alias(ty, scope))
            .and_then(|resolved| match resolved {
                TypeExpr::Applied { name, args }
                    if name == enum_name && args.len() == enum_info.type_params.len() =>
                {
                    Some(
                        enum_info
                            .type_params
                            .iter()
                            .map(|param| param.name.clone())
                            .zip(args)
                            .collect(),
                    )
                }
                _ => None,
            })
            .unwrap_or_default();
        let bindings: Vec<(String, InferredType)> = args
            .iter()
            .zip(&variant_info.fields)
            .filter_map(|(arg, field)| match &arg.node {
                Node::Identifier(name) if name != "_" => {
                    let field_type = field.type_expr.as_ref().map(|ty| {
                        if type_bindings.is_empty() {
                            ty.clone()
                        } else {
                            Self::apply_type_bindings(ty, &type_bindings)
                        }
                    });
                    let field_type = field_type
                        .filter(|ty| !Self::contains_type_param(ty, &unbound_param_names));
                    Some((name.clone(), field_type))
                }
                _ => None,
            })
            .collect();
        for (name, ty) in bindings {
            scope.define_var(&name, ty);
        }
    }
}
