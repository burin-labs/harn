use std::collections::BTreeMap;

use harn_parser::{substitute_type_expr, Node, SNode, ShapeField, TypeExpr};

use crate::chunk::{Chunk, Op};
use crate::value::VmValue;

use super::{peel_node, CompileError, Compiler};

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
    /// A position that constrains nothing.
    ///
    /// The validator dispatches on which keys a schema carries, so a schema
    /// with none of them reaches no check and accepts any value. This is the
    /// representation for a slot the compiler deliberately declines to
    /// constrain, as opposed to one it failed to lower.
    fn unconstrained() -> Self {
        SchemaFragment::Dict(BTreeMap::new())
    }

    /// Emission weight: the number of bytecode-emitting nodes in the fragment.
    /// A `Value` containing a dict or list still emits per-element construction
    /// ops, so its internal nodes count toward the budget.
    fn node_count(&self) -> usize {
        match self {
            SchemaFragment::Value(value) => vm_value_node_count(value),
            SchemaFragment::Ref(_) => 1,
            SchemaFragment::Apply { args, .. } => {
                2 + args.iter().map(SchemaFragment::node_count).sum::<usize>()
            }
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
            SchemaFragment::Apply { args, .. } => {
                1 + args
                    .iter()
                    .map(SchemaFragment::schema_depth)
                    .max()
                    .unwrap_or(0)
            }
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
            SchemaFragment::Apply { name, args } => SchemaFragment::Apply {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| arg.pruned_schema(depth_limit - 1))
                    .collect(),
            },
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
        VmValue::Dict(entries) => 1 + entries.values().map(vm_value_node_count).sum::<usize>(),
        VmValue::List(items) => 1 + items.iter().map(vm_value_node_count).sum::<usize>(),
        _ => 1,
    }
}

#[derive(Clone)]
pub(super) enum SchemaFragment {
    Value(VmValue),
    Ref(String),
    /// An imported generic exports a schema factory in its declaring lexical
    /// environment. Arguments are schemas resolved in the consumer's scope.
    Apply {
        name: String,
        args: Vec<SchemaFragment>,
    },
    Dict(BTreeMap<String, SchemaFragment>),
    List(Vec<SchemaFragment>),
}

impl Compiler {
    /// Compile each exported type schema into an independently addressable
    /// module initializer. Running these after imports makes referenced
    /// imported schemas ordinary lexical inputs while keeping the cached
    /// artifact immutable and relocatable. A module may export more schema
    /// bytecode than one u16-addressed chunk can hold; declaration-sized
    /// chunks remove that package-wide limit without weakening any schema.
    pub fn compile_public_type_schema_initializers(
        program: &[SNode],
        source_file: Option<String>,
    ) -> Result<Vec<Chunk>, CompileError> {
        Self::new().compile_type_schema_initializers(program, source_file, None)
    }

    /// Compile the selected exported type schemas. `None` preserves the
    /// caller-independent full-module behavior; a closed-program linker passes
    /// the exact runtime type names its consumers can observe.
    pub fn compile_selected_public_type_schema_initializers(
        program: &[SNode],
        source_file: Option<String>,
        selected_names: Option<&std::collections::BTreeSet<String>>,
    ) -> Result<Vec<Chunk>, CompileError> {
        Self::new().compile_type_schema_initializers(program, source_file, selected_names)
    }

    /// Materialize schemas with this compiler's resolved import catalog and
    /// options. The source-only entry points delegate to the same lowering.
    pub fn compile_type_schema_initializers(
        mut self,
        program: &[SNode],
        source_file: Option<String>,
        selected_names: Option<&std::collections::BTreeSet<String>>,
    ) -> Result<Vec<Chunk>, CompileError> {
        let compiler = &mut self;
        compiler.collect_type_aliases(program);
        let mut chunks = Vec::new();
        for sn in program {
            let Node::TypeDecl {
                name, is_pub: true, ..
            } = peel_node(sn)
            else {
                continue;
            };
            if selected_names.is_some_and(|selected| !selected.contains(name)) {
                continue;
            }
            compiler.chunk = Chunk::new();
            compiler.string_constants.clear();
            compiler.chunk.source_file.clone_from(&source_file);
            if compiler.emit_schema_for_alias(name)
                || compiler.emit_generic_schema_factory(name, source_file.clone())?
            {
                compiler.emit_define_binding(name, false);
                compiler.chunk.emit(Op::Nil, compiler.line);
                compiler.chunk.emit(Op::Return, compiler.line);
                super::ensure_chunk_addressable(
                    &compiler.chunk,
                    &format!("the public type-schema initializer for `{name}`"),
                    compiler.line,
                )?;
                chunks.push(std::mem::take(&mut compiler.chunk));
            }
        }
        Ok(chunks)
    }

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
        // A cycle-broken position lowers to no runtime constraint, the same
        // answer `expand_alias` already gives for the same cycle. Abandoning
        // the whole fragment instead meant a recursive alias published no
        // schema at all, and an importing module — which registers a selective
        // import body-less and so lowers it to a runtime load by name — found
        // nothing to load and failed at load time as `Undefined variable` on a
        // name that is a type.
        //
        // The outer shape stays constrained; only the recursive slot opens.
        if visiting.contains(guard_key) {
            return Some(SchemaFragment::unconstrained());
        }
        // The nesting cap is a different condition. It says the alias graph is
        // too deep to lower, not that it closed a cycle, so it stays a refusal
        // and a genuinely unmaterializable alias still fails at compile time
        // under a message naming it.
        if visiting.len() >= MAX_SCHEMA_ALIAS_NEST {
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
            // `Schema<T>` stores a schema dictionary, not a value of T. Its
            // static payload relation is owned by the typechecker. Preserve
            // that runtime representation without shadowing a local alias.
            TypeExpr::Applied { name, args }
                if name == "Schema" && args.len() == 1 && !self.type_aliases.contains_key(name) =>
            {
                Self::type_expr_to_schema_value(&TypeExpr::Named("dict".to_string()))
                    .map(SchemaFragment::Value)
            }
            TypeExpr::Applied { name, args } => {
                let alias = self.type_aliases.get(name)?;
                let Some(body) = alias.body.as_ref() else {
                    if args.len() > u8::MAX as usize {
                        return None;
                    }
                    return Some(SchemaFragment::Apply {
                        name: name.clone(),
                        args: args
                            .iter()
                            .map(|arg| self.schema_fragment_for_type(arg, visiting))
                            .collect::<Option<Vec<_>>>()?,
                    });
                };
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
        match fragment {
            SchemaFragment::Value(value) => self.emit_vm_value_literal(value),
            SchemaFragment::Ref(name) => self.emit_get_binding(name),
            SchemaFragment::Apply { name, args } => {
                self.emit_get_binding(name);
                for arg in args {
                    self.emit_schema_fragment(arg);
                }
                self.chunk.emit_u8(Op::Call, args.len() as u8, self.line);
            }
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

    /// Export generic aliases as ordinary lexical closures, using the same
    /// schema lowering as local concrete aliases. Private helper names stay
    /// owned by this module; no AST or duplicate type table crosses the import.
    pub(super) fn emit_generic_schema_factory(
        &mut self,
        name: &str,
        source_file: Option<String>,
    ) -> Result<bool, super::CompileError> {
        let Some(alias) = self.type_aliases.get(name).cloned() else {
            return Ok(false);
        };
        let Some(body) = alias.body else {
            return Ok(false);
        };
        if alias.type_params.is_empty() || alias.type_params.len() > u8::MAX as usize {
            return Ok(false);
        }
        let params = alias
            .type_params
            .iter()
            .map(|param| harn_parser::TypedParam::untyped(&param.name))
            .collect::<Vec<_>>();
        let mut compiler = self.nested_body();
        compiler.type_aliases = self.type_aliases.clone();
        for param in &params {
            compiler.type_aliases.insert(
                param.name.clone(),
                super::TypeAliasDefinition {
                    type_params: Vec::new(),
                    body: None,
                },
            );
        }
        compiler.declare_param_slots(&params);
        let Some(fragment) =
            compiler.schema_fragment_for_type(&body, &mut vec![TypeExpr::Named(name.to_string())])
        else {
            return Ok(false);
        };
        compiler.emit_schema_fragment(&fragment.bounded());
        compiler.chunk.emit(Op::Return, self.line);
        compiler.chunk.source_file = source_file;
        super::ensure_chunk_addressable(
            &compiler.chunk,
            &format!("generic schema `{name}`"),
            self.line,
        )?;
        let function = crate::CompiledFunction {
            name: format!("<schema:{name}>"),
            type_params: Vec::new(),
            nominal_type_names: Vec::new(),
            params: compiler.compile_param_slots(&params),
            default_start: None,
            chunk: std::sync::Arc::new(compiler.chunk),
            is_generator: false,
            is_stream: false,
            has_rest_param: false,
            has_runtime_type_checks: false,
        };
        let index = self.chunk.functions.len();
        self.chunk.functions.push(std::sync::Arc::new(function));
        self.chunk.emit_u16(Op::Closure, index as u16, self.line);
        Ok(true)
    }
}
