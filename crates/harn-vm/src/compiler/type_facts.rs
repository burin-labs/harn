use harn_parser::{BindingPattern, Node, SNode, TypeExpr, TypedParam};

use crate::chunk::Op;

use super::Compiler;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrimitiveType {
    Int,
    Float,
    Bool,
    String,
    Nil,
}

impl Compiler {
    pub(super) fn record_param_types(&mut self, params: &[TypedParam]) {
        for param in params {
            if let Some(type_expr) = &param.type_expr {
                self.define_type_fact(&param.name, type_expr.clone());
            }
        }
    }

    pub(super) fn record_binding_type(
        &mut self,
        pattern: &BindingPattern,
        type_expr: Option<TypeExpr>,
    ) {
        match pattern {
            BindingPattern::Identifier(name) => {
                if let Some(type_expr) = type_expr {
                    self.define_type_fact(name, type_expr);
                }
            }
            BindingPattern::Dict(fields) => {
                let Some(TypeExpr::Shape(shape_fields)) = type_expr else {
                    return;
                };
                for field in fields.iter().filter(|field| !field.is_rest) {
                    let Some(shape_field) =
                        shape_fields.iter().find(|shape| shape.name == field.key)
                    else {
                        continue;
                    };
                    let binding_name = field.alias.as_deref().unwrap_or(&field.key);
                    self.define_type_fact(binding_name, shape_field.type_expr.clone());
                }
            }
            BindingPattern::List(elements) => {
                let Some(TypeExpr::List(item_type)) = type_expr else {
                    return;
                };
                for element in elements {
                    let element_type = if element.is_rest {
                        TypeExpr::List(item_type.clone())
                    } else {
                        (*item_type).clone()
                    };
                    self.define_type_fact(&element.name, element_type);
                }
            }
            BindingPattern::Pair(first, second) => {
                let Some(TypeExpr::Applied { name, args }) = type_expr else {
                    return;
                };
                if name == "Pair" && args.len() == 2 {
                    self.define_type_fact(first, args[0].clone());
                    self.define_type_fact(second, args[1].clone());
                }
            }
        }
    }

    pub(super) fn assign_type_fact(&mut self, name: &str, type_expr: Option<TypeExpr>) {
        if let Some(type_expr) = type_expr {
            let type_expr = self.expand_alias(&type_expr);
            for scope in self.type_scopes.iter_mut().rev() {
                if let Some(existing) = scope.get_mut(name) {
                    if *existing == type_expr {
                        return;
                    }
                    let existing_kind = Self::primitive_kind(existing);
                    let new_kind = Self::primitive_kind(&type_expr);
                    if existing_kind.is_some() && existing_kind == new_kind {
                        *existing = type_expr;
                    } else {
                        scope.remove(name);
                    }
                    return;
                }
            }
        } else {
            for scope in self.type_scopes.iter_mut().rev() {
                if scope.remove(name).is_some() {
                    return;
                }
            }
        }
    }

    pub(super) fn infer_expr_type(&self, expr: &SNode) -> Option<TypeExpr> {
        match &expr.node {
            Node::IntLiteral(_) => Some(TypeExpr::Named("int".into())),
            Node::FloatLiteral(_) => Some(TypeExpr::Named("float".into())),
            Node::StringLiteral(_) | Node::RawStringLiteral(_) | Node::InterpolatedString(_) => {
                Some(TypeExpr::Named("string".into()))
            }
            Node::BoolLiteral(_) => Some(TypeExpr::Named("bool".into())),
            Node::NilLiteral => Some(TypeExpr::Named("nil".into())),
            Node::DurationLiteral(_) => Some(TypeExpr::Named("duration".into())),
            Node::Identifier(name) => self.lookup_type_fact(name),
            Node::UnaryOp { op, operand } => {
                let operand_type = self.infer_expr_type(operand)?;
                match op.as_str() {
                    "-" if matches!(
                        Self::primitive_kind(&self.expand_alias(&operand_type)),
                        Some(PrimitiveType::Int | PrimitiveType::Float)
                    ) =>
                    {
                        Some(operand_type)
                    }
                    "!" => Some(TypeExpr::Named("bool".into())),
                    _ => None,
                }
            }
            Node::BinaryOp { op, left, right } => {
                let left_type = self.infer_expr_type(left);
                let right_type = self.infer_expr_type(right);
                self.infer_binary_result_type(op, left_type.as_ref(), right_type.as_ref())
            }
            Node::Ternary {
                true_expr,
                false_expr,
                ..
            } => {
                let true_type = self.infer_expr_type(true_expr)?;
                let false_type = self.infer_expr_type(false_expr)?;
                if true_type == false_type {
                    Some(true_type)
                } else {
                    None
                }
            }
            Node::ListLiteral(items) => self.infer_list_literal_type(items),
            Node::DictLiteral(entries) => {
                let mut fields = Vec::new();
                for entry in entries {
                    let key = match &entry.key.node {
                        Node::Identifier(key) | Node::StringLiteral(key) => key.clone(),
                        _ => return Some(TypeExpr::Named("dict".into())),
                    };
                    let Some(type_expr) = self.infer_expr_type(&entry.value) else {
                        return Some(TypeExpr::Named("dict".into()));
                    };
                    fields.push(harn_parser::ShapeField {
                        name: key,
                        type_expr,
                        optional: false,
                    });
                }
                if fields.is_empty() {
                    Some(TypeExpr::Named("dict".into()))
                } else {
                    Some(TypeExpr::Shape(fields))
                }
            }
            Node::RangeExpr { .. } => Some(TypeExpr::Named("range".into())),
            _ => None,
        }
    }

    pub(super) fn infer_for_item_type(&self, iterable: &SNode) -> Option<TypeExpr> {
        match self.infer_expr_type(iterable)? {
            TypeExpr::List(item) | TypeExpr::Iter(item) => Some(*item),
            TypeExpr::DictType(key, value) => Some(TypeExpr::Applied {
                name: "Pair".into(),
                args: vec![*key, *value],
            }),
            TypeExpr::Named(name) if name == "range" => Some(TypeExpr::Named("int".into())),
            _ => None,
        }
    }

    pub(super) fn specialized_binary_op(
        &self,
        op: &str,
        left: Option<&TypeExpr>,
        right: Option<&TypeExpr>,
    ) -> Option<Op> {
        let left = Self::primitive_kind(&self.expand_alias(left?))?;
        let right = Self::primitive_kind(&self.expand_alias(right?))?;
        match (left, right) {
            (PrimitiveType::Int, PrimitiveType::Int) => match op {
                "+" => Some(Op::AddInt),
                "-" => Some(Op::SubInt),
                "*" => Some(Op::MulInt),
                "/" => Some(Op::DivInt),
                "%" => Some(Op::ModInt),
                "==" => Some(Op::EqualInt),
                "!=" => Some(Op::NotEqualInt),
                "<" => Some(Op::LessInt),
                ">" => Some(Op::GreaterInt),
                "<=" => Some(Op::LessEqualInt),
                ">=" => Some(Op::GreaterEqualInt),
                _ => None,
            },
            (PrimitiveType::Float, PrimitiveType::Float) => match op {
                "+" => Some(Op::AddFloat),
                "-" => Some(Op::SubFloat),
                "*" => Some(Op::MulFloat),
                "/" => Some(Op::DivFloat),
                "%" => Some(Op::ModFloat),
                "==" => Some(Op::EqualFloat),
                "!=" => Some(Op::NotEqualFloat),
                "<" => Some(Op::LessFloat),
                ">" => Some(Op::GreaterFloat),
                "<=" => Some(Op::LessEqualFloat),
                ">=" => Some(Op::GreaterEqualFloat),
                _ => None,
            },
            (PrimitiveType::Bool, PrimitiveType::Bool) => match op {
                "==" => Some(Op::EqualBool),
                "!=" => Some(Op::NotEqualBool),
                _ => None,
            },
            (PrimitiveType::String, PrimitiveType::String) => match op {
                "==" => Some(Op::EqualString),
                "!=" => Some(Op::NotEqualString),
                _ => None,
            },
            _ => None,
        }
    }

    fn define_type_fact(&mut self, name: &str, type_expr: TypeExpr) {
        if harn_parser::is_discard_name(name) {
            return;
        }
        let type_expr = self.expand_alias(&type_expr);
        if let Some(scope) = self.type_scopes.last_mut() {
            scope.insert(name.to_string(), type_expr);
        }
    }

    fn lookup_type_fact(&self, name: &str) -> Option<TypeExpr> {
        self.type_scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).cloned())
    }

    fn infer_list_literal_type(&self, items: &[SNode]) -> Option<TypeExpr> {
        let mut item_type: Option<TypeExpr> = None;
        for item in items {
            let inferred = self.infer_expr_type(item)?;
            item_type = Some(match item_type {
                None => inferred,
                Some(current) if current == inferred => current,
                Some(_) => return Some(TypeExpr::Named("list".into())),
            });
        }
        Some(TypeExpr::List(Box::new(
            item_type.unwrap_or_else(|| TypeExpr::Named("_".into())),
        )))
    }

    pub(super) fn infer_binary_result_type(
        &self,
        op: &str,
        left: Option<&TypeExpr>,
        right: Option<&TypeExpr>,
    ) -> Option<TypeExpr> {
        if matches!(op, "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||") {
            return Some(TypeExpr::Named("bool".into()));
        }
        let left = self.expand_alias(left?);
        let right = self.expand_alias(right?);
        let left_kind = Self::primitive_kind(&left);
        let right_kind = Self::primitive_kind(&right);

        match op {
            "+" => match (left_kind, right_kind) {
                (Some(PrimitiveType::Int), Some(PrimitiveType::Int)) => {
                    Some(TypeExpr::Named("int".into()))
                }
                (Some(PrimitiveType::Float), Some(PrimitiveType::Float))
                | (Some(PrimitiveType::Int), Some(PrimitiveType::Float))
                | (Some(PrimitiveType::Float), Some(PrimitiveType::Int)) => {
                    Some(TypeExpr::Named("float".into()))
                }
                (Some(PrimitiveType::String), Some(PrimitiveType::String)) => {
                    Some(TypeExpr::Named("string".into()))
                }
                _ => None,
            },
            "-" | "/" | "%" | "**" => match (left_kind, right_kind) {
                (Some(PrimitiveType::Int), Some(PrimitiveType::Int)) => {
                    Some(TypeExpr::Named("int".into()))
                }
                (Some(PrimitiveType::Float), Some(PrimitiveType::Float))
                | (Some(PrimitiveType::Int), Some(PrimitiveType::Float))
                | (Some(PrimitiveType::Float), Some(PrimitiveType::Int)) => {
                    Some(TypeExpr::Named("float".into()))
                }
                _ => None,
            },
            "*" => match (left_kind, right_kind) {
                (Some(PrimitiveType::Int), Some(PrimitiveType::Int)) => {
                    Some(TypeExpr::Named("int".into()))
                }
                (Some(PrimitiveType::Float), Some(PrimitiveType::Float))
                | (Some(PrimitiveType::Int), Some(PrimitiveType::Float))
                | (Some(PrimitiveType::Float), Some(PrimitiveType::Int)) => {
                    Some(TypeExpr::Named("float".into()))
                }
                (Some(PrimitiveType::String), Some(PrimitiveType::Int))
                | (Some(PrimitiveType::Int), Some(PrimitiveType::String)) => {
                    Some(TypeExpr::Named("string".into()))
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn primitive_kind(type_expr: &TypeExpr) -> Option<PrimitiveType> {
        match type_expr {
            TypeExpr::Named(name) => match name.as_str() {
                "int" => Some(PrimitiveType::Int),
                "float" => Some(PrimitiveType::Float),
                "bool" => Some(PrimitiveType::Bool),
                "string" => Some(PrimitiveType::String),
                "nil" => Some(PrimitiveType::Nil),
                _ => None,
            },
            TypeExpr::LitInt(_) => Some(PrimitiveType::Int),
            TypeExpr::LitString(_) => Some(PrimitiveType::String),
            _ => None,
        }
    }
}
