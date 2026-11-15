use std::collections::BTreeMap;
use std::sync::Arc;

use harn_parser::{ShapeField, TypeExpr};

use super::*;

/// One declared type with its prepared, shared schema and value codecs.
#[derive(Clone, Debug)]
pub struct PreparedWireType {
    graph: Arc<PreparedWireGraph>,
    root: WireTypeId,
}

impl PreparedWireType {
    pub(crate) fn from_graph(graph: PreparedWireGraph, root: WireTypeId) -> Self {
        Self {
            graph: Arc::new(graph),
            root,
        }
    }
    pub fn root(&self) -> &WireTypeId {
        &self.root
    }
    pub fn graph(&self) -> &PreparedWireGraph {
        &self.graph
    }
    /// Hydration shape. Runtime catalogs retain their original validation schema.
    pub fn json_schema(&self) -> serde_json::Value {
        self.graph
            .json_schema(&self.root)
            .expect("prepared root exists")
    }
    pub fn decode(&self, value: &serde_json::Value) -> Result<crate::VmValue, WireFailure> {
        self.graph.decode(&self.root, value)
    }
    pub fn encode(&self, value: &crate::VmValue) -> Result<serde_json::Value, WireFailure> {
        self.graph.encode(&self.root, value)
    }
}

impl crate::TypeSchemaResolver {
    pub fn prepare_wire_type(
        &self,
        ty: &TypeExpr,
        source: WireSourceRef,
    ) -> Result<PreparedWireType, WireFailure> {
        let mut builder = TypeBuilder {
            resolver: self,
            nodes: BTreeMap::new(),
        };
        let ty = self.wire_scope.qualify(ty);
        let root = builder.lower(&ty, source, 0)?;
        let graph = PreparedWireGraph::prepare(
            builder
                .nodes
                .into_values()
                .map(|node| node.expect("completed type preparation fills reservations")),
        )?;
        Ok(PreparedWireType {
            graph: Arc::new(graph),
            root,
        })
    }
}

struct TypeBuilder<'a> {
    resolver: &'a crate::TypeSchemaResolver,
    nodes: BTreeMap<WireTypeId, Option<WireType>>,
}

impl TypeBuilder<'_> {
    fn lower(
        &mut self,
        ty: &TypeExpr,
        source: WireSourceRef,
        depth: usize,
    ) -> Result<WireTypeId, WireFailure> {
        let reference = match ty {
            TypeExpr::Named(name) => Some((name.as_str(), &[][..])),
            TypeExpr::Applied { name, args } => Some((name.as_str(), args.as_slice())),
            _ => None,
        };
        let declaration = reference
            .and_then(|(name, _)| self.resolver.wire_scope.declarations.get(name))
            .cloned();
        let source = declaration
            .as_ref()
            .filter(|declaration| !matches!(declaration.source, WireSourceRef::Synthetic { .. }))
            .map_or(source, |declaration| declaration.source.clone());
        let id = WireTypeId(super::identity::digest(
            &serde_json::json!({"harn_type":ty}),
        ));
        if self.nodes.contains_key(&id) {
            return Ok(id);
        }
        let failure = |code| WireFailure {
            code,
            type_id: Some(id.clone()),
            source: source.clone(),
            structural_path: String::new(),
        };
        if depth > MAX_WIRE_VALUE_DEPTH {
            return Err(failure(WireFailureCode::RecursiveExpansion));
        }
        self.nodes.insert(id.clone(), None);
        let shape = if let Some(((_, args), declaration)) = reference.zip(declaration.as_ref()) {
            let parameters = &declaration.parameters;
            if parameters.len() != args.len() {
                return Err(failure(WireFailureCode::InvalidContract));
            }
            let bindings: BTreeMap<_, _> = parameters
                .iter()
                .zip(args)
                .map(|(parameter, argument)| (parameter.name.clone(), argument.clone()))
                .collect();
            let identity = WireDeclarationIdentity {
                declaration_id: declaration.id.clone(),
                name: declaration.name.clone(),
                arguments: args
                    .iter()
                    .map(|argument| self.lower(argument, source.clone(), depth + 1))
                    .collect::<Result<_, _>>()?,
            };
            match &declaration.kind {
                super::scope::DeclarationKind::Alias(body) => WireShape::Alias {
                    target: self.lower(
                        &harn_parser::substitute_type_expr(body, &bindings),
                        source.clone(),
                        depth + 1,
                    )?,
                    declaration: identity,
                },
                super::scope::DeclarationKind::Unsupported => {
                    return Err(failure(WireFailureCode::UnsupportedType))
                }
                super::scope::DeclarationKind::Struct(fields) => {
                    let fields: Vec<_> = fields
                        .iter()
                        .map(|field| {
                            Ok(ShapeField {
                                name: field.name.clone(),
                                optional: field.optional,
                                span: field.span,
                                type_expr: harn_parser::substitute_type_expr(
                                    field
                                        .type_expr
                                        .as_ref()
                                        .ok_or_else(|| failure(WireFailureCode::UnsupportedType))?,
                                    &bindings,
                                ),
                            })
                        })
                        .collect::<Result<_, WireFailure>>()?;
                    WireShape::Record {
                        fields: self.fields(&id, &fields, &source, depth)?,
                        additional: None,
                        nominal: Some(identity),
                    }
                }
                super::scope::DeclarationKind::Enum(variants) => {
                    let variants = variants
                        .iter()
                        .map(|variant| {
                            let variant_id = format!("{}:variant:{}", id.0, variant.name);
                            let fields: Vec<_> = variant
                                .fields
                                .iter()
                                .map(|field| {
                                    Ok(ShapeField {
                                        name: field.name.clone(),
                                        optional: false,
                                        span: field.span,
                                        type_expr: harn_parser::substitute_type_expr(
                                            field.type_expr.as_ref().ok_or_else(|| {
                                                failure(WireFailureCode::UnsupportedType)
                                            })?,
                                            &bindings,
                                        ),
                                    })
                                })
                                .collect::<Result<_, WireFailure>>()?;
                            Ok(WireEnumVariant {
                                id: variant_id.clone(),
                                name: variant.name.clone(),
                                fields: self.fields(
                                    &WireTypeId(variant_id),
                                    &fields,
                                    &source,
                                    depth,
                                )?,
                                source: field_source(&source, variant.span),
                                documentation: None,
                            })
                        })
                        .collect::<Result<_, WireFailure>>()?;
                    WireShape::Enum {
                        nominal: identity,
                        variants,
                    }
                }
            }
        } else {
            match ty {
                TypeExpr::Named(name) => match name.as_str() {
                    "nil" => WireShape::Null,
                    "bool" => WireShape::Boolean,
                    "int" => WireShape::Integer {
                        encoding: WireIntegerEncoding::JsonNumber,
                        minimum: i64::MIN.to_string(),
                        maximum: i64::MAX.to_string(),
                    },
                    "float" => WireShape::Float,
                    "string" => WireShape::String,
                    "bytes" => WireShape::Bytes {
                        encoding: WireBytesEncoding::Base64Envelope,
                    },
                    "decimal" => WireShape::Decimal {
                        encoding: WireDecimalEncoding::CanonicalString,
                    },
                    _ => return Err(failure(WireFailureCode::UnsupportedType)),
                },
                TypeExpr::LitString(value) => WireShape::Literal {
                    value: WireLiteral::String {
                        value: value.clone(),
                    },
                },
                TypeExpr::LitInt(value) => WireShape::Literal {
                    value: WireLiteral::Integer {
                        value: value.to_string(),
                    },
                },
                TypeExpr::List(item) => WireShape::List {
                    item: self.lower(item, source.clone(), depth + 1)?,
                },
                TypeExpr::Applied { name, args } if name == "set" && args.len() == 1 => {
                    WireShape::Set {
                        item: self.lower(&args[0], source.clone(), depth + 1)?,
                    }
                }
                TypeExpr::Tuple(items) => WireShape::Tuple {
                    items: items
                        .iter()
                        .map(|item| self.lower(item, source.clone(), depth + 1))
                        .collect::<Result<_, _>>()?,
                },
                TypeExpr::DictType(key, value) if matches!(key.as_ref(), TypeExpr::Named(name) if name == "string") => {
                    WireShape::Map {
                        value: self.lower(value, source.clone(), depth + 1)?,
                    }
                }
                TypeExpr::DictType(_, _) => {
                    return Err(failure(WireFailureCode::UnsupportedMapKey))
                }
                TypeExpr::Shape(fields) => WireShape::Record {
                    fields: self.fields(&id, fields, &source, depth)?,
                    additional: None,
                    nominal: None,
                },
                TypeExpr::Union(members) => WireShape::Union {
                    members: members
                        .iter()
                        .map(|member| self.lower(member, source.clone(), depth + 1))
                        .collect::<Result<_, _>>()?,
                },
                TypeExpr::Intersection(members) => {
                    let mut fields: BTreeMap<String, WireField> = BTreeMap::new();
                    for member in members {
                        let member = self.lower(member, source.clone(), depth + 1)?;
                        let member = self
                            .unaliased_node(&member)
                            .ok_or_else(|| failure(WireFailureCode::RecursiveExpansion))?;
                        let WireShape::Record {
                            fields: member_fields,
                            additional: None,
                            nominal: None,
                        } = &member.shape
                        else {
                            return Err(failure(WireFailureCode::UnsupportedType));
                        };
                        for field in member_fields.clone() {
                            if let Some(previous) = fields.get_mut(&field.name) {
                                if self.unaliased_node(&previous.type_id).map(|node| &node.id)
                                    != self.unaliased_node(&field.type_id).map(|node| &node.id)
                                {
                                    return Err(failure(WireFailureCode::UnsupportedType));
                                }
                                previous.optional &= field.optional;
                            } else {
                                let mut field = field;
                                field.id = format!("{}:field:{}", id.0, field.name);
                                fields.insert(field.name.clone(), field);
                            }
                        }
                    }
                    WireShape::Record {
                        fields: fields.into_values().collect(),
                        additional: None,
                        nominal: None,
                    }
                }
                TypeExpr::Owned(inner) => WireShape::Alias {
                    target: self.lower(inner, source.clone(), depth + 1)?,
                    declaration: WireDeclarationIdentity {
                        declaration_id: "harn:owned".into(),
                        name: "owned".into(),
                        arguments: Vec::new(),
                    },
                },
                _ => return Err(failure(WireFailureCode::UnsupportedType)),
            }
        };
        self.nodes.insert(
            id.clone(),
            Some(WireType {
                id: id.clone(),
                name: declaration
                    .map(|declaration| declaration.name)
                    .or_else(|| reference.map(|(name, _)| name.to_owned())),
                source,
                documentation: None,
                shape,
            }),
        );
        Ok(id)
    }

    fn unaliased_node<'b>(&'b self, id: &WireTypeId) -> Option<&'b WireType> {
        let mut node = self.nodes.get(id)?.as_ref()?;
        for _ in 0..=MAX_WIRE_VALUE_DEPTH {
            match &node.shape {
                WireShape::Alias { target, .. } => node = self.nodes.get(target)?.as_ref()?,
                _ => return Some(node),
            }
        }
        None
    }

    fn fields(
        &mut self,
        parent: &WireTypeId,
        fields: &[ShapeField],
        source: &WireSourceRef,
        depth: usize,
    ) -> Result<Vec<WireField>, WireFailure> {
        fields
            .iter()
            .map(|field| {
                let source = field_source(source, field.span);
                Ok(WireField {
                    id: format!("{}:field:{}", parent.0, field.name),
                    name: field.name.clone(),
                    type_id: self.lower(&field.type_expr, source.clone(), depth + 1)?,
                    optional: field.optional,
                    source,
                    documentation: None,
                })
            })
            .collect()
    }
}

fn field_source(parent: &WireSourceRef, span: harn_lexer::Span) -> WireSourceRef {
    match parent {
        WireSourceRef::TextSpan { document, .. } => WireSourceRef::TextSpan {
            document: document.clone(),
            start: span.start.try_into().unwrap_or(u32::MAX),
            end: span.end.try_into().unwrap_or(u32::MAX),
        },
        _ => parent.clone(),
    }
}
