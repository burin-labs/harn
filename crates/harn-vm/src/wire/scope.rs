//! Resolved declaration identities supplied by the module graph, without I/O.

use std::collections::BTreeMap;

use harn_parser::{Node, SNode, TypeExpr, TypeParam};

use super::{WireFailure, WireFailureCode, WireSourceRef};

/// Import lookup is complete before the VM receives this record.
#[derive(Clone, Debug)]
pub struct ResolvedWireDeclaration {
    pub id: String,
    pub source: WireSourceRef,
    pub declaration: SNode,
    pub bindings: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct ResolvedWireScope {
    pub source: WireSourceRef,
    pub declarations: Vec<ResolvedWireDeclaration>,
    pub bindings: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub(super) enum DeclarationKind {
    Alias(TypeExpr),
    Struct(Vec<harn_parser::StructField>),
    Enum(Vec<harn_parser::EnumVariant>),
    Unsupported,
}

#[derive(Clone, Debug)]
pub(super) struct Declaration {
    pub id: String,
    pub name: String,
    pub source: WireSourceRef,
    pub parameters: Vec<TypeParam>,
    pub kind: DeclarationKind,
}

#[derive(Clone, Debug)]
pub(crate) struct DeclarationScope {
    pub(crate) source: WireSourceRef,
    pub(super) declarations: BTreeMap<String, Declaration>,
    pub(super) bindings: BTreeMap<String, String>,
}

impl DeclarationScope {
    pub(crate) fn from_program(program: &[SNode]) -> Self {
        let visible: BTreeMap<_, _> = program
            .iter()
            .filter_map(|node| declaration_name(node).map(|name| (name.to_owned(), node.clone())))
            .collect();
        let bindings: BTreeMap<_, _> = visible
            .keys()
            .map(|name| (name.clone(), format!("program::{name}")))
            .collect();
        Self::prepare(ResolvedWireScope {
            source: WireSourceRef::Synthetic {
                owner: "program".into(),
            },
            bindings: bindings.clone(),
            declarations: visible
                .into_iter()
                .map(|(name, node)| ResolvedWireDeclaration {
                    id: bindings[&name].clone(),
                    source: WireSourceRef::Synthetic {
                        owner: format!("program::{name}"),
                    },
                    declaration: node,
                    bindings: bindings.clone(),
                })
                .collect(),
        })
        .expect("one local declaration per visible name is prepared deterministically")
    }

    pub(crate) fn prepare(scope: ResolvedWireScope) -> Result<Self, WireFailure> {
        let mut declarations = BTreeMap::new();
        for site in scope.declarations {
            let (_, node) = harn_parser::peel_attributes(&site.declaration);
            let failure = || WireFailure {
                code: WireFailureCode::InvalidContract,
                type_id: None,
                source: site.source.clone(),
                structural_path: String::new(),
            };
            let Some(name) = declaration_name(node) else {
                return Err(failure());
            };
            if site.id.is_empty()
                || declarations.contains_key(&site.id)
                || super::graph::validate_source(&site.source).is_err()
            {
                return Err(failure());
            }
            let parameters = match &node.node {
                Node::TypeDecl { type_params, .. }
                | Node::StructDecl { type_params, .. }
                | Node::EnumDecl { type_params, .. }
                | Node::InterfaceDecl { type_params, .. } => type_params.clone(),
                _ => Vec::new(),
            };
            let mut bindings = site.bindings;
            for parameter in &parameters {
                bindings.remove(&parameter.name);
            }
            let kind = match &node.node {
                Node::TypeDecl { type_expr, .. } => {
                    DeclarationKind::Alias(qualify(type_expr, &bindings))
                }
                Node::StructDecl { fields, .. } => DeclarationKind::Struct(
                    fields
                        .iter()
                        .map(|field| harn_parser::StructField {
                            type_expr: field.type_expr.as_ref().map(|ty| qualify(ty, &bindings)),
                            ..field.clone()
                        })
                        .collect(),
                ),
                Node::EnumDecl { variants, .. } => DeclarationKind::Enum(
                    variants
                        .iter()
                        .map(|variant| harn_parser::EnumVariant {
                            fields: variant
                                .fields
                                .iter()
                                .map(|field| harn_parser::TypedParam {
                                    type_expr: field
                                        .type_expr
                                        .as_ref()
                                        .map(|ty| qualify(ty, &bindings)),
                                    ..field.clone()
                                })
                                .collect(),
                            ..variant.clone()
                        })
                        .collect(),
                ),
                _ => DeclarationKind::Unsupported,
            };
            declarations.insert(
                site.id.clone(),
                Declaration {
                    id: site.id,
                    name: name.into(),
                    source: site.source,
                    parameters,
                    kind,
                },
            );
        }
        super::graph::validate_source(&scope.source).map_err(|()| WireFailure {
            code: WireFailureCode::InvalidContract,
            type_id: None,
            source: scope.source.clone(),
            structural_path: String::new(),
        })?;
        Ok(Self {
            source: scope.source,
            declarations,
            bindings: scope.bindings,
        })
    }

    pub(crate) fn qualify(&self, ty: &TypeExpr) -> TypeExpr {
        qualify(ty, &self.bindings)
    }
}

impl Default for DeclarationScope {
    fn default() -> Self {
        Self {
            source: WireSourceRef::Synthetic {
                owner: "program".into(),
            },
            declarations: BTreeMap::new(),
            bindings: BTreeMap::new(),
        }
    }
}

impl crate::TypeSchemaResolver {
    pub fn from_resolved_wire_scope(scope: ResolvedWireScope) -> Result<Self, WireFailure> {
        let program: Vec<_> = scope
            .declarations
            .iter()
            .map(|site| site.declaration.clone())
            .collect();
        let mut resolver = Self::from_program(&program);
        resolver.wire_scope = DeclarationScope::prepare(scope)?;
        Ok(resolver)
    }
}

pub fn declaration_name(node: &SNode) -> Option<&str> {
    let (_, node) = harn_parser::peel_attributes(node);
    match &node.node {
        Node::TypeDecl { name, .. }
        | Node::StructDecl { name, .. }
        | Node::EnumDecl { name, .. }
        | Node::InterfaceDecl { name, .. } => Some(name),
        _ => None,
    }
}

fn qualify(ty: &TypeExpr, bindings: &BTreeMap<String, String>) -> TypeExpr {
    let q = |ty: &TypeExpr| qualify(ty, bindings);
    match ty {
        TypeExpr::Named(name) => TypeExpr::Named(bindings.get(name).unwrap_or(name).clone()),
        TypeExpr::Applied { name, args } => TypeExpr::Applied {
            name: bindings.get(name).unwrap_or(name).clone(),
            args: args.iter().map(q).collect(),
        },
        TypeExpr::List(item) => TypeExpr::List(Box::new(q(item))),
        TypeExpr::DictType(key, value) => TypeExpr::DictType(Box::new(q(key)), Box::new(q(value))),
        TypeExpr::Tuple(items) => TypeExpr::Tuple(items.iter().map(q).collect()),
        TypeExpr::Union(items) => TypeExpr::Union(items.iter().map(q).collect()),
        TypeExpr::Intersection(items) => TypeExpr::Intersection(items.iter().map(q).collect()),
        TypeExpr::Shape(fields) => TypeExpr::Shape(
            fields
                .iter()
                .map(|field| harn_parser::ShapeField {
                    type_expr: q(&field.type_expr),
                    ..field.clone()
                })
                .collect(),
        ),
        TypeExpr::OpenShape { fields, rests } => TypeExpr::OpenShape {
            fields: fields
                .iter()
                .map(|field| harn_parser::ShapeField {
                    type_expr: q(&field.type_expr),
                    ..field.clone()
                })
                .collect(),
            rests: rests.iter().map(q).collect(),
        },
        TypeExpr::Owned(item) => TypeExpr::Owned(Box::new(q(item))),
        TypeExpr::Iter(item) => TypeExpr::Iter(Box::new(q(item))),
        TypeExpr::Generator(item) => TypeExpr::Generator(Box::new(q(item))),
        TypeExpr::Stream(item) => TypeExpr::Stream(Box::new(q(item))),
        TypeExpr::FnType {
            params,
            return_type,
        } => TypeExpr::FnType {
            params: params.iter().map(q).collect(),
            return_type: Box::new(q(return_type)),
        },
        _ => ty.clone(),
    }
}
