//! Infer one parameter's type from how the program already uses it.
//!
//! Evidence comes from two directions. Inside the declaration, the body says
//! what the value must support: the members it reads, the methods it calls,
//! and the typed parameters it is handed on to. Outside it, every call site in
//! the module graph says what the value actually is.
//!
//! Each source is a tier. The strongest tier that has any evidence decides the
//! type, and it decides only when it is unanimous. Anything else is `unknown`,
//! reported with the reason, because a wrong annotation is worse than an
//! honest validated boundary.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use harn_parser::param_annotations::UnannotatedParam;
use harn_parser::typechecker::{format_type, method_registry};
use harn_parser::{visit, BindingPattern, Node, SNode, TypeExpr};

/// Why a parameter got the annotation it got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Cause {
    /// The body reaches a declared capability through the value, so it is the
    /// runtime's `Harness` handle.
    CapabilityHandle,
    /// The body hands the value to a parameter that already has a type.
    ForwardedToTypedParameter,
    /// The methods called on the value belong to exactly one builtin type.
    ReceiverMethods,
    /// An operator the body applies to the value fixes its type.
    MatchedOperand,
    /// Runtime checks make the parameter a validated `unknown` boundary.
    RuntimeTypeChecks,
    /// Every call site in the module graph passes the same type.
    CallSites,
    /// The body returns the value, so the declared return type is its type.
    ReturnedDirectly,
    /// The body iterates the value.
    Iterated,
    /// The parameter's default value fixes the type.
    DefaultValue,
    /// The body never reads the parameter, so the safe top type is sufficient.
    UnusedParameter,
    /// Evidence exists but disagrees.
    ConflictingEvidence,
    /// The body reads named members off the value, which makes it a dict but
    /// says nothing about the field types.
    MemberReads,
    /// Nothing fixes a concrete type, so the migration preserves the old
    /// unchecked boundary explicitly.
    NoEvidence,
    /// An inferred type was rejected because it made the file stop checking.
    RejectedByRecheck,
}

impl Cause {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Cause::CapabilityHandle => "capability reached through the value",
            Cause::ForwardedToTypedParameter => "forwarded to a typed parameter",
            Cause::ReceiverMethods => "methods identify the receiver type",
            Cause::MatchedOperand => "an operator fixes the type",
            Cause::RuntimeTypeChecks => "runtime checks validate an unknown boundary",
            Cause::CallSites => "call sites agree",
            Cause::ReturnedDirectly => "returned directly",
            Cause::Iterated => "iterated by the body",
            Cause::DefaultValue => "default value",
            Cause::UnusedParameter => "parameter is unused",
            Cause::ConflictingEvidence => "evidence disagreed",
            Cause::MemberReads => "member reads make it a dict",
            Cause::NoEvidence => "no concrete type",
            Cause::RejectedByRecheck => "inferred type did not check",
        }
    }

    /// Whether this evidence source can produce a useful concrete type.
    pub(super) const fn is_inferred(self) -> bool {
        matches!(
            self,
            Cause::CapabilityHandle
                | Cause::ForwardedToTypedParameter
                | Cause::ReceiverMethods
                | Cause::MatchedOperand
                | Cause::RuntimeTypeChecks
                | Cause::CallSites
                | Cause::ReturnedDirectly
                | Cause::Iterated
                | Cause::DefaultValue
                | Cause::UnusedParameter
                | Cause::MemberReads
        )
    }
}

/// Identity of one parameter across the whole module graph.
type SiteKey = (usize, usize, String);

/// One module's resolved callable names.
#[derive(Debug, Default)]
pub(super) struct ModuleResolution {
    pub(super) callables: HashMap<String, usize>,
    pub(super) namespaces: HashMap<String, usize>,
}

impl ModuleResolution {
    fn callable(&self, name: &str) -> Option<CallableKey> {
        self.callables
            .get(name)
            .map(|module| (*module, name.to_string()))
    }

    fn namespace_callable(&self, namespace: &str, name: &str) -> Option<CallableKey> {
        self.namespaces
            .get(namespace)
            .map(|module| (*module, name.to_string()))
    }
}

type CallableKey = (usize, String);
type ParameterKey = (usize, String, usize);

/// Types this pass has already inferred, keyed by resolved callable and
/// parameter position. Feeding them back turns one pass into a fixed point.
pub(super) type SettledTypes = HashMap<ParameterKey, String>;

/// What one call site passed in one argument position.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ArgKind {
    Known(String),
    Nil,
    /// Optional access can yield nil, while the property's non-nil type is
    /// not available from the untyped caller expression.
    MaybeNil,
    Unknown,
}

/// Everything one declaration's body says about one of its parameters.
#[derive(Debug, Default, Clone)]
struct BodyEvidence {
    /// The body reads the parameter as a value, including inside string
    /// interpolation.
    referenced: bool,
    /// `(callee, argument index)` pairs the value is handed on to, where the
    /// callee is a plain function or builtin name.
    forwards: Vec<(Option<CallableKey>, String, usize)>,
    /// `(capability field, method, argument index)` triples the value is
    /// handed on to through `harness.<capability>.<method>(...)`. Most stdlib
    /// work reaches the host this way, so these carry more of the migration
    /// than plain calls do.
    capability_forwards: Vec<(String, String, usize)>,
    methods: BTreeSet<String>,
    fields: BTreeSet<String>,
    /// A method is invoked through a capability field on this value, such as
    /// `harness.fs.read_text(...)`. A field name alone is not enough: ordinary
    /// records legitimately have fields such as `tools` and `runtime`.
    capability_handle: bool,
    /// Types the value is combined or compared with by an operator whose
    /// operands must match.
    operands: Vec<ArgKind>,
    /// Nil comparisons at the boundary. These widen concrete evidence to an
    /// optional type; unlike a positive `type_of` branch, a nil guard commonly
    /// models the absence case for the whole function.
    accepted_runtime_types: BTreeSet<String>,
    /// Runtime kinds inspected with `type_of`. A check proves that the body is
    /// a normalizer, not that the checked kind is the whole accepted domain:
    /// `if type_of(value) == "bool" { ... }` may be one branch among many.
    /// Keep that distinction structural so the fixer chooses validated
    /// `unknown` instead of inventing an unsound closed union.
    inspected_runtime_types: BTreeSet<String>,
    /// The body returns the value directly, so the declared return type is
    /// also the parameter's type.
    returned: bool,
    iterated: bool,
    /// The body rebinds or shadows the name, so nothing below the declaration
    /// is reliable evidence about the parameter itself.
    shadowed: bool,
}

/// Facts one module contributes to the whole-program inference.
#[derive(Debug, Default)]
pub(super) struct ModuleFacts {
    /// Argument kinds seen at every call, keyed by callee and position.
    call_args: BTreeMap<ParameterKey, Vec<ArgKind>>,
    /// Declared parameter types, keyed by callee and position. These are the
    /// forwarding targets that already carry an annotation.
    declared_params: BTreeMap<ParameterKey, String>,
    /// Body evidence, keyed by module, the owning declaration's start offset,
    /// and the parameter name. The module is part of the key because two files
    /// routinely hold a declaration at the same byte offset.
    body: HashMap<SiteKey, BodyEvidence>,
    /// Default-value kinds, keyed the same way.
    defaults: HashMap<SiteKey, ArgKind>,
    /// Declared return types, keyed by module and declaration start.
    returns: HashMap<(usize, usize), String>,
}

impl ModuleFacts {
    pub(super) fn merge(&mut self, other: ModuleFacts) {
        for (key, mut kinds) in other.call_args {
            self.call_args.entry(key).or_default().append(&mut kinds);
        }
        self.declared_params.extend(other.declared_params);
        self.body.extend(other.body);
        self.defaults.extend(other.defaults);
        self.returns.extend(other.returns);
    }
}

/// Read one module's contribution to the shared evidence tables.
///
/// `module` identifies the file; it is part of every per-site key so evidence
/// from one file cannot overwrite another's.
pub(super) fn collect(
    module: usize,
    source: &str,
    program: &[SNode],
    settled: &SettledTypes,
    resolution: &ModuleResolution,
) -> ModuleFacts {
    let mut facts = ModuleFacts::default();
    visit::walk_program(program, &mut |node| {
        let (name, params, body) = match &node.node {
            Node::FnDecl {
                name, params, body, ..
            }
            | Node::Pipeline {
                name, params, body, ..
            }
            | Node::ToolDecl {
                name, params, body, ..
            } => (name, params, body),
            _ => return,
        };
        let mut env: HashMap<String, String> = HashMap::new();
        let mut untyped: HashSet<String> = HashSet::new();
        for (index, param) in params.iter().enumerate() {
            match &param.type_expr {
                Some(ty) => {
                    let rendered = format_type(ty);
                    facts
                        .declared_params
                        .insert((module, name.clone(), index), rendered.clone());
                    env.insert(param.name.clone(), rendered);
                }
                None => {
                    // A type this pass already inferred in an earlier round
                    // counts as declared from here on. That is what lets a
                    // parameter typed by its call sites go on to type the
                    // parameters it is handed to.
                    if let Some(rendered) = settled.get(&(module, name.clone(), index)) {
                        facts
                            .declared_params
                            .insert((module, name.clone(), index), rendered.clone());
                        env.insert(param.name.clone(), rendered.clone());
                    }
                    untyped.insert(param.name.clone());
                    if let Some(default) = &param.default_value {
                        facts.defaults.insert(
                            (module, node.span.start, param.name.clone()),
                            arg_kind(default, &env),
                        );
                    }
                }
            }
        }
        let declared_return = match &node.node {
            Node::FnDecl { return_type, .. }
            | Node::Pipeline { return_type, .. }
            | Node::ToolDecl { return_type, .. } => return_type.as_ref().and_then(concrete_type),
            _ => None,
        };
        collect_body(
            module,
            node.span.start,
            source,
            body,
            &env,
            &untyped,
            resolution,
            &mut facts,
        );
        if let Some(rendered) = declared_return {
            facts.returns.insert((module, node.span.start), rendered);
        }
    });
    facts
}

fn collect_body(
    module: usize,
    owner_start: usize,
    source: &str,
    body: &[SNode],
    env: &HashMap<String, String>,
    untyped: &HashSet<String>,
    resolution: &ModuleResolution,
    facts: &mut ModuleFacts,
) {
    let mut evidence: HashMap<String, BodyEvidence> = untyped
        .iter()
        .map(|name| (name.clone(), BodyEvidence::default()))
        .collect();
    // Locals join the environment as they are bound, so a chain like
    // `const v = to_int(n)` followed by `v < lo` still types `lo`.
    let mut env = env.clone();
    visit::walk_program_interpolated(source, body, &mut |node| {
        if let Node::Identifier(name) = &node.node {
            if let Some(entry) = evidence.get_mut(name) {
                entry.referenced = true;
            }
        }
        match &node.node {
            Node::BinaryOp { op, left, right } if op == "??" => {
                // `value ?? fallback` types `value` as the fallback's type
                // widened with nil: the operator exists because it can be nil.
                if let ArgKind::Known(rendered) = arg_kind(right, &env) {
                    if let Some(entry) = target(left, &mut evidence) {
                        entry
                            .operands
                            .push(ArgKind::Known(nullable_type(&rendered)));
                    }
                }
            }
            Node::BinaryOp { op, left, right } => {
                collect_runtime_type_check(op, left, right, &mut evidence);
                collect_runtime_type_check(op, right, left, &mut evidence);
                if !matches_operands(op) {
                    return;
                }
                if let ArgKind::Known(rendered) = arg_kind(right, &env) {
                    if let Some(entry) = target(left, &mut evidence) {
                        entry.operands.push(ArgKind::Known(rendered));
                    }
                }
                if let ArgKind::Known(rendered) = arg_kind(left, &env) {
                    if let Some(entry) = target(right, &mut evidence) {
                        entry.operands.push(ArgKind::Known(rendered));
                    }
                }
            }
            Node::PropertyAccess { object, property }
            | Node::OptionalPropertyAccess { object, property } => {
                if let Some(entry) = target(object, &mut evidence) {
                    entry.fields.insert(property.clone());
                }
            }
            Node::MethodCall {
                object,
                method,
                args,
            }
            | Node::OptionalMethodCall {
                object,
                method,
                args,
            } => {
                if let Some(entry) = target(object, &mut evidence) {
                    entry.methods.insert(method.clone());
                }
                if let Some(handle) = capability_handle(object) {
                    if let Some(entry) = target(handle, &mut evidence) {
                        entry.capability_handle = true;
                    }
                }
                if let Some(field) = capability_field(object) {
                    for (index, arg) in args.iter().enumerate() {
                        if let Some(entry) = target(arg, &mut evidence) {
                            entry
                                .capability_forwards
                                .push((field.clone(), method.clone(), index));
                        }
                    }
                }
                if let Node::Identifier(namespace) = &object.node {
                    if let Some(callee) = resolution.namespace_callable(namespace, method) {
                        for (index, arg) in args.iter().enumerate() {
                            facts
                                .call_args
                                .entry((callee.0, callee.1.clone(), index))
                                .or_default()
                                .push(arg_kind(arg, &env));
                            if let Some(entry) = target(arg, &mut evidence) {
                                entry
                                    .forwards
                                    .push((Some(callee.clone()), method.clone(), index));
                            }
                        }
                    }
                }
            }
            Node::ReturnStmt { value: Some(value) } => {
                if let Some(entry) = target(value, &mut evidence) {
                    entry.returned = true;
                }
            }
            Node::ForIn {
                pattern, iterable, ..
            } => {
                if let Some(entry) = target(iterable, &mut evidence) {
                    entry.iterated = true;
                }
                shadow(pattern, &mut evidence);
            }
            Node::LetBinding {
                pattern,
                type_ann,
                value,
                ..
            }
            | Node::ConstBinding {
                pattern,
                type_ann,
                value,
                ..
            } => {
                shadow(pattern, &mut evidence);
                if let BindingPattern::Identifier(name) = pattern {
                    let rendered = match type_ann {
                        Some(ty) => concrete_type(ty),
                        None => match arg_kind(value, &env) {
                            ArgKind::Known(rendered) => Some(rendered),
                            _ => None,
                        },
                    };
                    match rendered {
                        Some(rendered) => env.insert(name.clone(), rendered),
                        // A rebound name must not keep an older type.
                        None => env.remove(name),
                    };
                }
            }
            Node::Closure { params, .. } => {
                for param in params {
                    if let Some(entry) = evidence.get_mut(&param.name) {
                        entry.shadowed = true;
                    }
                }
            }
            Node::FunctionCall { name, args, .. } => {
                let callee = resolution.callable(name);
                for (index, arg) in args.iter().enumerate() {
                    if let Some(callee) = &callee {
                        facts
                            .call_args
                            .entry((callee.0, callee.1.clone(), index))
                            .or_default()
                            .push(arg_kind(arg, &env));
                    }
                    if let Some(entry) = target(arg, &mut evidence) {
                        entry.forwards.push((callee.clone(), name.clone(), index));
                    }
                }
            }
            _ => {}
        }
    });
    for (name, found) in evidence {
        facts.body.insert((module, owner_start, name), found);
    }
}

/// Whether both operands of this operator must be the same type, so one side
/// tells you the other.
///
/// Equality also contributes ordinary same-type evidence. Comparisons with
/// nil are handled separately by [`collect_runtime_type_check`], because they
/// widen the eventual answer instead of claiming that the parameter is only
/// nil.
fn matches_operands(op: &str) -> bool {
    matches!(
        op,
        "+" | "-" | "*" | "/" | "%" | "<" | "<=" | ">" | ">=" | "==" | "!="
    )
}

fn collect_runtime_type_check(
    op: &str,
    candidate: &SNode,
    expected: &SNode,
    evidence: &mut HashMap<String, BodyEvidence>,
) {
    if !matches!(op, "==" | "!=") {
        return;
    }
    if matches!(expected.node, Node::NilLiteral) {
        if let Some(entry) = target(candidate, evidence) {
            entry.accepted_runtime_types.insert("nil".to_string());
        }
        return;
    }
    let Node::FunctionCall { name, args, .. } = &candidate.node else {
        return;
    };
    if name != "type_of" || args.len() != 1 {
        return;
    }
    let Some(runtime_type) = string_literal(expected) else {
        return;
    };
    if !matches!(
        runtime_type,
        "string" | "int" | "float" | "bool" | "nil" | "list" | "dict" | "closure" | "bytes"
    ) {
        return;
    }
    if let Some(entry) = target(&args[0], evidence) {
        entry
            .inspected_runtime_types
            .insert(runtime_type.to_string());
    }
}

fn string_literal(node: &SNode) -> Option<&str> {
    match &node.node {
        Node::StringLiteral(value) | Node::RawStringLiteral(value) => Some(value),
        _ => None,
    }
}

/// The capability field name in a `<handle>.<field>` receiver, when the field
/// names a declared capability.
fn capability_field(object: &SNode) -> Option<String> {
    let Node::PropertyAccess { property, .. } = &object.node else {
        return None;
    };
    harn_builtin_meta::CapabilityId::from_field_name(property)?;
    Some(property.clone())
}

/// The handle in `<handle>.<capability>.<method>(...)`.
fn capability_handle(object: &SNode) -> Option<&SNode> {
    let (handle, property) = match &object.node {
        Node::PropertyAccess { object, property }
        | Node::OptionalPropertyAccess { object, property } => (object.as_ref(), property),
        _ => return None,
    };
    harn_builtin_meta::CapabilityId::from_field_name(property)?;
    Some(handle)
}

fn target<'a>(
    node: &SNode,
    evidence: &'a mut HashMap<String, BodyEvidence>,
) -> Option<&'a mut BodyEvidence> {
    let Node::Identifier(name) = &node.node else {
        return None;
    };
    evidence.get_mut(name)
}

fn shadow(pattern: &BindingPattern, evidence: &mut HashMap<String, BodyEvidence>) {
    if let BindingPattern::Identifier(name) = pattern {
        if let Some(entry) = evidence.get_mut(name) {
            entry.shadowed = true;
        }
    }
}

fn arg_kind(node: &SNode, env: &HashMap<String, String>) -> ArgKind {
    match &node.node {
        Node::StringLiteral(_) | Node::RawStringLiteral(_) | Node::InterpolatedString(_) => {
            ArgKind::Known("string".into())
        }
        Node::IntLiteral(_) => ArgKind::Known("int".into()),
        Node::FloatLiteral(_) => ArgKind::Known("float".into()),
        Node::BoolLiteral(_) => ArgKind::Known("bool".into()),
        Node::ListLiteral(_) => ArgKind::Known("list".into()),
        Node::DictLiteral(_) => ArgKind::Known("dict".into()),
        Node::StructConstruct { struct_name, .. } => ArgKind::Known(struct_name.clone()),
        Node::EnumConstruct { enum_name, .. } => ArgKind::Known(enum_name.clone()),
        // An unannotated closure's exact signature may require contextual
        // typing, but its nominal callable kind is still certain. This is
        // enough to migrate optional callback seams from `callback = nil` to
        // `callback: closure? = nil` instead of freezing them at `nil` or
        // widening them to `unknown`.
        Node::Closure { .. } => ArgKind::Known("closure".into()),
        Node::Ternary {
            true_expr,
            false_expr,
            ..
        } => match (arg_kind(true_expr, env), arg_kind(false_expr, env)) {
            (ArgKind::Known(left), ArgKind::Known(right)) if left == right => ArgKind::Known(left),
            (ArgKind::Nil, ArgKind::Known(right)) | (ArgKind::Known(right), ArgKind::Nil) => {
                ArgKind::Known(nullable_type(&right))
            }
            _ => ArgKind::Unknown,
        },
        Node::NilLiteral => ArgKind::Nil,
        Node::OptionalPropertyAccess { .. } => ArgKind::MaybeNil,
        Node::Identifier(name) => env
            .get(name)
            .filter(|rendered| !matches!(rendered.as_str(), "any" | "unknown"))
            .cloned()
            .map_or(ArgKind::Unknown, ArgKind::Known),
        Node::MethodCall { object, method, .. } => capability_field(object)
            .and_then(|field| harn_builtin_meta::CapabilityId::from_field_name(&field))
            .and_then(|capability| {
                harn_parser::builtin_signatures::lookup_capability_method(capability, method)
            })
            .and_then(|signature| {
                concrete_type(&harn_parser::builtin_signatures::ty_to_type_expr(
                    &signature.returns,
                ))
            })
            .map_or(ArgKind::Unknown, ArgKind::Known),
        Node::FunctionCall { name, .. } => {
            harn_parser::builtin_signatures::builtin_return_type(name)
                .as_ref()
                .and_then(concrete_type)
                .map_or(ArgKind::Unknown, ArgKind::Known)
        }
        _ => ArgKind::Unknown,
    }
}

/// A rendered type, unless it is one the annotation would gain nothing from.
fn concrete_type(ty: &TypeExpr) -> Option<String> {
    useful(format_type(ty))
}

/// Drop the rendered types that constrain nothing at a declaration boundary:
/// `any` itself, `nil` alone, and a bare generic parameter belonging to some
/// other signature.
fn useful(rendered: String) -> Option<String> {
    let useless = rendered.is_empty()
        || rendered == "any"
        || rendered == "unknown"
        || rendered == "nil"
        || (rendered.len() == 1 && rendered.chars().all(char::is_uppercase));
    (!useless).then_some(rendered)
}

/// The type to write for one parameter, and the reason.
#[derive(Debug, Clone)]
pub(super) struct Inference {
    pub(super) rendered: String,
    pub(super) cause: Cause,
}

impl Inference {
    /// The outcome for a parameter whose inferred type failed the re-check.
    pub(super) fn rejected() -> Self {
        Inference {
            rendered: "unknown".to_string(),
            cause: Cause::RejectedByRecheck,
        }
    }
}

/// Resolve one parameter against the whole-program evidence.
///
/// Tiers are tried strongest first. A tier with no evidence falls through to
/// the next one; a tier whose evidence splits stops the search and reports the
/// disagreement, because a lower tier cannot settle a contradiction a higher
/// one already found.
pub(super) fn infer(module: usize, param: &UnannotatedParam, facts: &ModuleFacts) -> Inference {
    let key = (module, param.owner_span.start, param.name.clone());
    let body = facts.body.get(&key).filter(|found| !found.shadowed);

    let calls = facts
        .call_args
        .get(&(module, param.owner.clone(), param.index))
        .cloned()
        .unwrap_or_default();
    let finish = |inference| {
        vetted(
            widen_with_default(
                widen_with_runtime_types(inference, body),
                facts.defaults.get(&key),
            ),
            &calls,
        )
    };

    if let Some(found) = body {
        if found.capability_handle {
            return finish(Inference {
                rendered: "Harness".to_string(),
                cause: Cause::CapabilityHandle,
            });
        }
        let forwarded = forwarded_types(found, facts);
        if let Some(result) = tier(forwarded, Cause::ForwardedToTypedParameter) {
            return finish(result);
        }
        if let Some(rendered) = receiver_type(&found.methods) {
            return finish(Inference {
                rendered,
                cause: Cause::ReceiverMethods,
            });
        }
        if let Some(result) = tier(found.operands.clone(), Cause::MatchedOperand) {
            return finish(result);
        }
    }

    if let Some(result) = tier(calls.clone(), Cause::CallSites) {
        return finish(result);
    }

    if body.is_some_and(|found| found.returned) {
        if let Some(rendered) = facts.returns.get(&(module, param.owner_span.start)) {
            return finish(Inference {
                rendered: rendered.clone(),
                cause: Cause::ReturnedDirectly,
            });
        }
    }

    if body.is_some_and(|found| found.iterated) {
        return finish(Inference {
            rendered: "list".to_string(),
            cause: Cause::Iterated,
        });
    }

    if let Some(ArgKind::Known(rendered)) = facts.defaults.get(&key) {
        return finish(Inference {
            rendered: rendered.clone(),
            cause: Cause::DefaultValue,
        });
    }

    if body.is_some_and(|found| !found.referenced) {
        return Inference {
            rendered: "unknown".to_string(),
            cause: Cause::UnusedParameter,
        };
    }

    if body
        .is_some_and(|found| !found.fields.is_empty() && found.inspected_runtime_types.is_empty())
    {
        return finish(Inference {
            rendered: "dict".to_string(),
            cause: Cause::MemberReads,
        });
    }

    if body.is_some_and(|found| {
        found
            .accepted_runtime_types
            .iter()
            .any(|runtime_type| runtime_type != "nil")
            && found.inspected_runtime_types.is_empty()
    }) {
        return finish(Inference {
            rendered: render_union(
                body.unwrap()
                    .accepted_runtime_types
                    .iter()
                    .cloned()
                    .collect(),
            ),
            cause: Cause::RuntimeTypeChecks,
        });
    }

    if body.is_some_and(|found| !found.inspected_runtime_types.is_empty()) {
        return Inference {
            rendered: "unknown".to_string(),
            cause: Cause::RuntimeTypeChecks,
        };
    }

    Inference {
        rendered: "unknown".to_string(),
        cause: Cause::NoEvidence,
    }
}

fn widen_with_runtime_types(mut inference: Inference, body: Option<&BodyEvidence>) -> Inference {
    let Some(body) = body else {
        return inference;
    };
    if body.accepted_runtime_types.is_empty()
        || matches!(inference.rendered.as_str(), "any" | "unknown")
    {
        return inference;
    }
    let mut members = union_members(&inference.rendered);
    for runtime_type in &body.accepted_runtime_types {
        if runtime_type != "nil" && !members.contains(runtime_type) {
            members.push(runtime_type.clone());
        }
    }
    if body.accepted_runtime_types.contains("nil") && !members.iter().any(|item| item == "nil") {
        members.push("nil".to_string());
    }
    inference.rendered = render_union(members);
    inference
}

/// Preserve a nullable default after stronger evidence identifies the non-nil
/// type. A default of `nil` cannot determine a type by itself, but dropping it
/// after body or call-site inference would write a signature that rejects the
/// declaration's own default.
fn widen_with_default(mut inference: Inference, default: Option<&ArgKind>) -> Inference {
    if matches!(default, Some(ArgKind::Nil | ArgKind::MaybeNil))
        && !matches!(inference.rendered.as_str(), "any" | "unknown")
    {
        inference.rendered = nullable_type(&inference.rendered);
    }
    inference
}

/// Reject a type the body argued for when a caller already passes something
/// else.
///
/// Every tier except the call-site tier reasons from inside the declaration,
/// so on its own it can annotate a signature that a caller in another module
/// then fails against. Writing the annotation is what makes those callers
/// checked, so the veto runs before the annotation exists rather than after
/// the migration has broken the build.
fn vetted(mut inference: Inference, calls: &[ArgKind]) -> Inference {
    if calls.contains(&ArgKind::MaybeNil)
        && !matches!(inference.rendered.as_str(), "any" | "unknown")
    {
        inference.rendered = nullable_type(&inference.rendered);
    }
    let members = union_members(&inference.rendered)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let contradicted = calls.iter().any(|kind| match kind {
        ArgKind::Known(passed) => {
            passed != &inference.rendered
                && !union_members(passed)
                    .iter()
                    .all(|member| members.contains(member))
        }
        ArgKind::Nil => !members.contains("nil"),
        ArgKind::MaybeNil => false,
        ArgKind::Unknown => false,
    });
    if contradicted {
        return Inference {
            rendered: "unknown".to_string(),
            cause: Cause::ConflictingEvidence,
        };
    }
    inference
}

/// One tier's verdict: a type when its evidence is unanimous, `unknown` when it
/// splits, and `None` when the tier saw nothing and the search should go on.
fn tier(kinds: Vec<ArgKind>, cause: Cause) -> Option<Inference> {
    if kinds.is_empty() || kinds.contains(&ArgKind::Unknown) {
        // An argument whose type nothing knows is not a disagreement. Falling
        // through lets a weaker tier answer instead of spending the parameter
        // on `unknown`.
        return None;
    }
    Some(match unanimous(kinds) {
        Some(rendered) => Inference { rendered, cause },
        None => Inference {
            rendered: "unknown".to_string(),
            cause: Cause::ConflictingEvidence,
        },
    })
}

/// The declared types of every typed parameter this value is handed on to.
///
/// A target whose own type is `any` or a bare generic is dropped rather than
/// counted as unknown: it constrains nothing, so letting it veto the tier
/// would throw away the targets that do constrain something.
fn forwarded_types(found: &BodyEvidence, facts: &ModuleFacts) -> Vec<ArgKind> {
    let capability = found
        .capability_forwards
        .iter()
        .filter_map(|(field, method, index)| {
            let capability = harn_builtin_meta::CapabilityId::from_field_name(field)?;
            let signature =
                harn_parser::builtin_signatures::lookup_capability_method(capability, method)?;
            let param = signature.params.get(*index)?;
            let rendered =
                concrete_type(&harn_parser::builtin_signatures::ty_to_type_expr(&param.ty))?;
            Some(ArgKind::Known(rendered))
        });
    found
        .forwards
        .iter()
        .filter_map(|(callee, name, index)| {
            if let Some(callee) = callee {
                if let Some(declared) =
                    facts
                        .declared_params
                        .get(&(callee.0, callee.1.clone(), *index))
                {
                    return useful(declared.clone()).map(ArgKind::Known);
                }
            }
            let signature = harn_parser::builtin_signatures::lookup(name)?;
            let param = signature.params.get(*index)?;
            let rendered =
                concrete_type(&harn_parser::builtin_signatures::ty_to_type_expr(&param.ty))?;
            Some(ArgKind::Known(rendered))
        })
        .chain(capability)
        .collect()
}

/// The builtin type whose method list covers every method called here.
fn receiver_type(methods: &BTreeSet<String>) -> Option<String> {
    if methods.is_empty() {
        return None;
    }
    let tables: [(&str, &[&str]); 4] = [
        ("string", method_registry::STRING_METHODS),
        ("list", method_registry::LIST_METHODS),
        ("dict", method_registry::DICT_METHODS),
        ("set", method_registry::SET_METHODS),
    ];
    let mut candidates: Vec<&str> = tables
        .iter()
        .filter(|(_, table)| {
            methods
                .iter()
                .all(|method| table.contains(&method.as_str()))
        })
        .map(|(name, _)| *name)
        .collect();
    candidates.dedup();
    match candidates.as_slice() {
        [only] => Some((*only).to_string()),
        _ => None,
    }
}

/// One type when every known argument agrees, widened with `| nil` when a call
/// site passes nil. `None` when something is unknown or the evidence splits.
fn unanimous(kinds: Vec<ArgKind>) -> Option<String> {
    if kinds.is_empty() {
        return None;
    }
    let nullable = kinds
        .iter()
        .any(|kind| matches!(kind, ArgKind::Nil | ArgKind::MaybeNil));
    let known: BTreeSet<&String> = kinds
        .iter()
        .filter_map(|kind| match kind {
            ArgKind::Known(rendered) => Some(rendered),
            _ => None,
        })
        .collect();
    let mut known = known.into_iter();
    let only = known.next()?;
    if known.next().is_some() {
        return None;
    }
    Some(if nullable {
        nullable_type(only)
    } else {
        only.clone()
    })
}

/// Expand the source shorthand into the semantic members used by inference.
/// Keeping this normalization here prevents each evidence tier from growing a
/// second, subtly different understanding of optional types.
fn union_members(rendered: &str) -> Vec<String> {
    if let Some(inner) = rendered.strip_suffix('?') {
        return vec![inner.to_string(), "nil".to_string()];
    }
    rendered.split(" | ").map(str::to_string).collect()
}

/// Render one semantic union in the language's canonical source form.
fn render_union(members: Vec<String>) -> String {
    let mut seen = BTreeSet::new();
    let mut members = members
        .into_iter()
        .filter(|member| seen.insert(member.clone()))
        .collect::<Vec<_>>();
    if let Some(nil) = members.iter().position(|member| member == "nil") {
        let nil = members.remove(nil);
        members.push(nil);
    }
    let non_nil = members
        .iter()
        .filter(|member| member.as_str() != "nil")
        .collect::<Vec<_>>();
    if members.iter().any(|member| member == "nil") && non_nil.len() == 1 {
        return format!("{}?", non_nil[0]);
    }
    members.join(" | ")
}

fn nullable_type(rendered: &str) -> String {
    render_union(
        union_members(rendered)
            .into_iter()
            .chain(std::iter::once("nil".to_string()))
            .collect(),
    )
}
