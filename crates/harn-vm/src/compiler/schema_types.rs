use std::collections::BTreeMap;

use harn_parser::{substitute_type_expr, ShapeField, TypeExpr};

use crate::chunk::Op;
use crate::value::VmValue;

use super::Compiler;

/// Backstop bound on how deeply alias materialization may nest before giving
/// up. Ordinary cycles are caught structurally by the `visiting` stack; this
/// guards the pathological case of a generic whose argument keeps growing on
/// each instantiation (e.g. `type Grow<T> = {next?: Grow<list<T>>}`), where no
/// two instantiations are ever equal, so the structural check alone would never
/// terminate.
const MAX_SCHEMA_ALIAS_NEST: usize = 128;

/// Ceiling on the number of fragment nodes a single alias schema may emit.
///
/// Fragments are expanded inline at every emission site (value-position uses,
/// schema guards, the module type-schema initializer), and a function body can
/// address at most 64 KiB of bytecode. A deep alias graph — a real-world
/// OpenAPI document model is ~40 mutually nested aliases — can otherwise lower
/// to hundreds of KiB. Over-budget fragments are pruned from the deepest
/// levels to permissive empty schemas: validation gets coarser (exactly the
/// pre-#5275 behavior, which degraded nested positions far more aggressively),
/// never wrong, and the emitted size is bounded at every site.
const SCHEMA_FRAGMENT_NODE_BUDGET: usize = 4096;

impl SchemaFragment {
    /// Fold a fragment tree containing no [`SchemaFragment::Ref`] into a
    /// single constant value; `None` when any nested position needs a runtime
    /// binding lookup.
    fn as_constant(&self) -> Option<VmValue> {
        match self {
            SchemaFragment::Value(value) => Some(value.clone()),
            SchemaFragment::Ref(_) => None,
            SchemaFragment::Dict(entries) => {
                let mut folded = std::collections::BTreeMap::new();
                for (key, value) in entries {
                    folded.insert(key.clone(), value.as_constant()?);
                }
                Some(VmValue::dict(folded))
            }
            SchemaFragment::List(items) => {
                let folded = items
                    .iter()
                    .map(SchemaFragment::as_constant)
                    .collect::<Option<Vec<_>>>()?;
                Some(VmValue::List(std::sync::Arc::new(folded)))
            }
        }
    }

    /// Emission weight: how many bytecode-emitting nodes the fragment expands
    /// to. A folded `Value` still emits per-element construction ops, so its
    /// internal dict/list nodes are counted rather than treated as one unit.
    fn node_count(&self) -> usize {
        match self {
            SchemaFragment::Value(value) => vm_value_node_count(value),
            SchemaFragment::Ref(_) => 1,
            SchemaFragment::Dict(entries) => {
                1 + entries
                    .values()
                    .map(SchemaFragment::node_count)
                    .sum::<usize>()
            }
            SchemaFragment::List(items) => {
                1 + items.iter().map(SchemaFragment::node_count).sum::<usize>()
            }
        }
    }

    /// Nesting depth in SCHEMA levels (each schema-position descent costs 1;
    /// structural carriers like a `properties` map or a `union` list are part
    /// of their schema level, not a level of their own).
    fn schema_depth(&self) -> usize {
        match self {
            SchemaFragment::Value(_) | SchemaFragment::Ref(_) => 1,
            SchemaFragment::Dict(entries) => {
                1 + entries
                    .iter()
                    .map(|(key, value)| match (key.as_str(), value) {
                        ("properties", SchemaFragment::Dict(fields)) => fields
                            .values()
                            .map(SchemaFragment::schema_depth)
                            .max()
                            .unwrap_or(0),
                        ("union" | "all_of", SchemaFragment::List(members)) => members
                            .iter()
                            .map(SchemaFragment::schema_depth)
                            .max()
                            .unwrap_or(0),
                        ("items" | "additional_properties", schema) => schema.schema_depth(),
                        _ => 0,
                    })
                    .max()
                    .unwrap_or(0)
            }
            SchemaFragment::List(_) => 1,
        }
    }

    /// Replace every nested SCHEMA position deeper than `depth_limit` with the
    /// permissive empty schema (`{}` accepts any value). Only positions the
    /// validator reads as a schema are replaced — the value under
    /// `properties.<field>`, each `union`/`all_of` member, `items`, and
    /// `additional_properties` — so structural carriers keep their required
    /// shape (a `union` value stays a list) and data keys (`type`, `required`)
    /// are never touched.
    fn pruned_schema(&self, depth_limit: usize) -> SchemaFragment {
        if depth_limit == 0 {
            return SchemaFragment::Value(VmValue::dict(std::collections::BTreeMap::<
                String,
                VmValue,
            >::new()));
        }
        match self {
            SchemaFragment::Value(_) | SchemaFragment::Ref(_) => self.clone(),
            SchemaFragment::Dict(entries) => SchemaFragment::Dict(
                entries
                    .iter()
                    .map(|(key, value)| {
                        let pruned = match (key.as_str(), value) {
                            ("properties", SchemaFragment::Dict(fields)) => SchemaFragment::Dict(
                                fields
                                    .iter()
                                    .map(|(field, schema)| {
                                        (field.clone(), schema.pruned_schema(depth_limit - 1))
                                    })
                                    .collect(),
                            ),
                            ("union" | "all_of", SchemaFragment::List(members)) => {
                                SchemaFragment::List(
                                    members
                                        .iter()
                                        .map(|member| member.pruned_schema(depth_limit - 1))
                                        .collect(),
                                )
                            }
                            ("items" | "additional_properties", schema) => {
                                schema.pruned_schema(depth_limit - 1)
                            }
                            _ => value.clone(),
                        };
                        (key.clone(), pruned)
                    })
                    .collect(),
            ),
            SchemaFragment::List(_) => self.clone(),
        }
    }

    /// Bound the fragment to [`SCHEMA_FRAGMENT_NODE_BUDGET`] nodes by pruning
    /// the deepest schema levels to permissive empty schemas. Pruning only
    /// ever weakens validation (a permissive subtree accepts what the full
    /// subtree accepted, and more), so a bounded fragment stays a sound schema
    /// for the alias — the tradeoff the pre-#5275 lowering also made, far more
    /// coarsely.
    fn bounded(self) -> SchemaFragment {
        if self.node_count() <= SCHEMA_FRAGMENT_NODE_BUDGET {
            return self;
        }
        let mut limit = self.schema_depth();
        while limit > 1 {
            limit -= 1;
            let candidate = self.pruned_schema(limit);
            if candidate.node_count() <= SCHEMA_FRAGMENT_NODE_BUDGET {
                return candidate;
            }
        }
        self.pruned_schema(1)
    }
}

/// Count the construction nodes a folded constant expands to at emission.
fn vm_value_node_count(value: &VmValue) -> usize {
    match value {
        VmValue::Dict(entries) => {
            1 + entries
                .iter()
                .map(|(_, v)| vm_value_node_count(v))
                .sum::<usize>()
        }
        VmValue::List(items) => 1 + items.iter().map(vm_value_node_count).sum::<usize>(),
        _ => 1,
    }
}

#[derive(Clone)]
pub(super) enum SchemaFragment {
    Value(VmValue),
    Ref(String),
    Dict(BTreeMap<String, SchemaFragment>),
    List(Vec<SchemaFragment>),
}

impl Compiler {
    pub(super) fn schema_fragment_for_alias(&self, name: &str) -> Option<SchemaFragment> {
        self.unbounded_schema_fragment_for_alias(name)
            .map(SchemaFragment::bounded)
    }

    fn unbounded_schema_fragment_for_alias(&self, name: &str) -> Option<SchemaFragment> {
        let alias = self.type_aliases.get(name)?;
        if !alias.type_params.is_empty() {
            return None;
        }
        match alias.body.as_ref() {
            Some(body) => self.schema_fragment_for_type(body, &mut Vec::new()),
            None => Some(SchemaFragment::Ref(name.to_string())),
        }
    }

    /// Recurse into an alias body under a cycle/depth guard keyed on the alias
    /// NAME (as a `TypeExpr::Named`). Name-keying deliberately treats a generic
    /// alias reused at distinct arguments on one path — e.g. `RefOr<Header>`
    /// nesting `RefOr<Example>` — as a cycle, so the inner reuse degrades at
    /// the nearest tolerant position (an unconstrained dict) instead of
    /// materializing. That is the pre-regression contract: fragments are
    /// expanded INLINE, so materializing every distinct instantiation
    /// duplicates subtrees combinatorially (a real-world OpenAPI alias graph
    /// compiled to ~417 KiB of initializer bytecode, past the 64 KiB chunk a
    /// jump operand can address). Keying on the applied form is only viable
    /// once fragments can be shared/interned instead of inlined — tracked on
    /// the materialization issue.
    fn schema_fragment_under_guard(
        &self,
        guard_key: &TypeExpr,
        body: &TypeExpr,
        visiting: &mut Vec<TypeExpr>,
    ) -> Option<SchemaFragment> {
        if visiting.len() >= MAX_SCHEMA_ALIAS_NEST || visiting.contains(guard_key) {
            return None;
        }
        visiting.push(guard_key.clone());
        let fragment = self.schema_fragment_for_type(body, visiting);
        visiting.pop();
        fragment
    }

    fn schema_fragment_for_type(
        &self,
        ty: &TypeExpr,
        visiting: &mut Vec<TypeExpr>,
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
                if !alias.type_params.is_empty() {
                    return None;
                }
                self.schema_fragment_under_guard(ty, body, visiting)
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
            TypeExpr::DictType(key, value) if matches!(key.as_ref(), TypeExpr::Named(name) if name == "string") =>
            {
                let mut entries = BTreeMap::from([(
                    "type".to_string(),
                    SchemaFragment::Value(VmValue::String(arcstr::ArcStr::from("dict"))),
                )]);
                // A dict whose value type cannot be lowered degrades to an
                // unconstrained dict instead of collapsing the whole alias, so
                // an alias that merely nests an un-materializable value still
                // yields a runtime schema (and keeps its value binding).
                if let Some(value_fragment) = self.schema_fragment_for_type(value, visiting) {
                    entries.insert("additional_properties".to_string(), value_fragment);
                }
                Some(SchemaFragment::Dict(entries))
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
                if alias.type_params.len() != args.len() {
                    return None;
                }
                let bindings = alias
                    .type_params
                    .iter()
                    .zip(args)
                    .map(|(param, arg)| (param.name.clone(), arg.clone()))
                    .collect();
                let instantiated = substitute_type_expr(body, &bindings);
                // Key on the alias name, not the applied form — see
                // `schema_fragment_under_guard` for why distinct instantiations
                // must not each materialize inline.
                let guard_key = TypeExpr::Named(name.clone());
                self.schema_fragment_under_guard(&guard_key, &instantiated, visiting)
            }
            TypeExpr::Owned(inner) => self.schema_fragment_for_type(inner, visiting),
            _ => None,
        }
    }

    fn schema_fragment_for_shape(
        &self,
        fields: &[ShapeField],
        additional: Option<&TypeExpr>,
        visiting: &mut Vec<TypeExpr>,
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
        // A fragment tree with no `Ref` is pure data: fold it into ONE
        // constant instead of emitting per-key construction ops. This is what
        // keeps a large all-local alias graph's schema initializer small — a
        // real-world OpenAPI module emitted ~100 KiB of BuildDict/BuildList
        // bytecode without the fold, past the 64 KiB a jump operand can
        // address, while the folded form is a handful of constant loads.
        if let Some(constant) = fragment.as_constant() {
            self.emit_vm_value_literal(&constant);
            return;
        }
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
