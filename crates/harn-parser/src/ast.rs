use harn_lexer::{Span, StringSegment};

/// A node wrapped with source location information.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Spanned<T> {
    pub node: T,
    pub span: Span,
}

impl<T> Spanned<T> {
    pub fn new(node: T, span: Span) -> Self {
        Self { node, span }
    }

    pub fn dummy(node: T) -> Self {
        Self {
            node,
            span: Span::dummy(),
        }
    }
}

/// A spanned AST node — the primary unit throughout the compiler.
pub type SNode = Spanned<Node>;

/// Helper to wrap a node with a span.
pub fn spanned(node: Node, span: Span) -> SNode {
    SNode::new(node, span)
}

/// If `node` is an `AttributedDecl`, returns `(attrs, inner)`; otherwise
/// returns an empty attribute slice and the node itself. Use at the top
/// of any consumer that processes top-level statements so attributes
/// flow through transparently.
pub fn peel_attributes(node: &SNode) -> (&[Attribute], &SNode) {
    match &node.node {
        Node::AttributedDecl { attributes, inner } => (attributes.as_slice(), inner.as_ref()),
        _ => (&[], node),
    }
}

/// A single argument to an attribute. Positional args have `name = None`;
/// named args use `name: Some("key")`. Values are restricted to
/// compile-time metadata expressions by the parser (literal scalars,
/// identifiers, lists, dicts, and call-shaped sentinels).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AttributeArg {
    pub name: Option<String>,
    pub value: SNode,
    pub span: Span,
}

/// An attribute attached to a declaration: `@deprecated(since: "0.8")`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Attribute {
    pub name: String,
    pub args: Vec<AttributeArg>,
    pub span: Span,
}

impl Attribute {
    /// Find a named argument by key.
    pub fn named_arg(&self, key: &str) -> Option<&SNode> {
        self.args
            .iter()
            .find(|a| a.name.as_deref() == Some(key))
            .map(|a| &a.value)
    }

    /// First positional argument, if any.
    pub fn positional(&self, idx: usize) -> Option<&SNode> {
        self.args
            .iter()
            .filter(|a| a.name.is_none())
            .nth(idx)
            .map(|a| &a.value)
    }

    /// Convenience: extract a string-literal arg by name.
    pub fn string_arg(&self, key: &str) -> Option<String> {
        match self.named_arg(key).map(|n| &n.node) {
            Some(Node::StringLiteral(s)) => Some(s.clone()),
            Some(Node::RawStringLiteral(s)) => Some(s.clone()),
            _ => None,
        }
    }
}

/// AST nodes for the Harn language.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum Node {
    /// A declaration carrying one or more attributes (`@attr`). The inner
    /// node is always one of: FnDecl, ToolDecl, Pipeline, StructDecl,
    /// EnumDecl, TypeDecl, InterfaceDecl, ImplBlock.
    AttributedDecl {
        attributes: Vec<Attribute>,
        inner: Box<SNode>,
    },
    Pipeline {
        name: String,
        params: Vec<String>,
        return_type: Option<TypeExpr>,
        /// Declared exception channel: `throws E` / `throws (E1 | E2)`, parsed
        /// as a single [`TypeExpr`] (a `throws (E1 | E2)` clause is a
        /// [`TypeExpr::Union`]). `None` leaves the callable's thrown-type set
        /// unconstrained — the historical default, so the annotation is purely
        /// additive and no existing code is forced to declare it.
        throws: Option<TypeExpr>,
        body: Vec<SNode>,
        extends: Option<String>,
        is_pub: bool,
    },
    /// `let PATTERN [: Type] = EXPR` — a **mutable** binding (reassignable).
    ///
    /// This is the TypeScript-aligned `let`: a normal block-scoped mutable
    /// variable. (Before the const/let keyword re-platform, `let` was
    /// immutable and `var` was the mutable form; `var` has been removed and
    /// its mutable role is now `let`.)
    LetBinding {
        pattern: BindingPattern,
        type_ann: Option<TypeExpr>,
        value: Box<SNode>,
        /// `true` for a top-level `pub let` — the binding's value is exported
        /// as part of the module's public surface (bound by value in
        /// importers, like every other cross-module value). Always `false` for
        /// block-scoped bindings.
        is_pub: bool,
    },
    /// `const PATTERN [: Type] = EXPR` — an **immutable** binding.
    ///
    /// The TypeScript-aligned `const`: the default, immutable binding form
    /// (reassignment is rejected). When the initializer falls in the pure,
    /// bounded const-eval subset (literal arithmetic, string concat, literal
    /// lists/dicts, ternaries, reads of earlier `const` identifiers, and a
    /// whitelist of pure builtins) it is **folded at compile time** via
    /// `harn_parser::const_eval`; otherwise it is an ordinary immutable
    /// runtime binding. Unlike the pre-re-platform `const`, an impure
    /// initializer is *not* an error (it simply is not folded), and a
    /// destructuring `pattern` is permitted (only a plain identifier pattern
    /// is eligible for folding). At runtime a folded binding re-evaluates the
    /// same expression so the value matches the compile-time fold byte-for-byte.
    ConstBinding {
        pattern: BindingPattern,
        type_ann: Option<TypeExpr>,
        value: Box<SNode>,
        /// `true` for a top-level `pub const` — the (compile-time-folded or
        /// runtime) value is exported as part of the module's public surface.
        /// Always `false` for block-scoped bindings.
        is_pub: bool,
    },
    OverrideDecl {
        name: String,
        params: Vec<String>,
        body: Vec<SNode>,
    },
    ImportDecl {
        path: String,
        /// When true, the wildcard import is a re-export: every public symbol
        /// from the target module becomes part of this module's public surface.
        is_pub: bool,
    },
    /// Selective import: import { foo, bar } from "module"
    SelectiveImport {
        names: Vec<String>,
        path: String,
        /// When true, the listed names are re-exported as part of this
        /// module's public surface.
        is_pub: bool,
    },
    EnumDecl {
        name: String,
        type_params: Vec<TypeParam>,
        variants: Vec<EnumVariant>,
        is_pub: bool,
    },
    StructDecl {
        name: String,
        type_params: Vec<TypeParam>,
        fields: Vec<StructField>,
        is_pub: bool,
    },
    InterfaceDecl {
        name: String,
        type_params: Vec<TypeParam>,
        associated_types: Vec<AssociatedType>,
        methods: Vec<InterfaceMethod>,
    },
    /// Impl block: impl TypeName { fn method(self, ...) { ... } ... }
    ImplBlock {
        type_name: String,
        methods: Vec<SNode>,
    },

    IfElse {
        condition: Box<SNode>,
        then_body: Vec<SNode>,
        else_body: Option<Vec<SNode>>,
    },
    ForIn {
        pattern: BindingPattern,
        iterable: Box<SNode>,
        body: Vec<SNode>,
    },
    MatchExpr {
        value: Box<SNode>,
        arms: Vec<MatchArm>,
    },
    WhileLoop {
        condition: Box<SNode>,
        body: Vec<SNode>,
    },
    Retry {
        count: Box<SNode>,
        body: Vec<SNode>,
    },
    /// Scoped cost-aware LLM routing block:
    /// `cost_route { key: value ... body }`.
    ///
    /// Options are inherited by nested `llm_call` invocations unless a
    /// call explicitly overrides the same option.
    CostRoute {
        options: Vec<(String, SNode)>,
        body: Vec<SNode>,
    },
    ReturnStmt {
        value: Option<Box<SNode>>,
    },
    TryCatch {
        body: Vec<SNode>,
        has_catch: bool,
        error_var: Option<String>,
        error_type: Option<TypeExpr>,
        catch_body: Vec<SNode>,
        finally_body: Option<Vec<SNode>>,
    },
    /// Try expression: try { body } — returns Result.Ok(value), an existing Result,
    /// or Result.Err(error).
    TryExpr {
        body: Vec<SNode>,
    },
    FnDecl {
        name: String,
        type_params: Vec<TypeParam>,
        params: Vec<TypedParam>,
        return_type: Option<TypeExpr>,
        /// Declared exception channel `throws E` / `throws (E1 | E2)`; see the
        /// [`Node::Pipeline`] `throws` field. `None` = unconstrained.
        throws: Option<TypeExpr>,
        where_clauses: Vec<WhereClause>,
        body: Vec<SNode>,
        is_pub: bool,
        is_stream: bool,
    },
    ToolDecl {
        name: String,
        description: Option<String>,
        params: Vec<TypedParam>,
        return_type: Option<TypeExpr>,
        /// Declared exception channel; see the [`Node::Pipeline`] `throws`
        /// field. `None` = unconstrained.
        throws: Option<TypeExpr>,
        body: Vec<SNode>,
        is_pub: bool,
    },
    /// Top-level `skill NAME { ... }` declaration.
    ///
    /// Skills bundle metadata, tool references, MCP server lists, and
    /// optional lifecycle hooks into a typed unit. Each body entry is a
    /// `<field_name> <expression>` pair; the compiler lowers the decl to
    /// `skill_define(skill_registry(), NAME, { field: expr, ... })` and
    /// binds the resulting registry dict to `NAME`.
    SkillDecl {
        name: String,
        fields: Vec<(String, SNode)>,
        is_pub: bool,
    },
    /// Top-level `eval_pack NAME_OR_STRING { ... }` declaration.
    ///
    /// The compiler lowers fields into `eval_pack_manifest({ ... })` and
    /// binds the normalized manifest to `binding_name`. Optional executable
    /// body statements are only run when the declaration itself is executed
    /// in script/block position; top-level pipeline preloading registers the
    /// manifest data without running the body.
    EvalPackDecl {
        binding_name: String,
        pack_id: String,
        fields: Vec<(String, SNode)>,
        body: Vec<SNode>,
        summarize: Option<Vec<SNode>>,
        is_pub: bool,
    },
    TypeDecl {
        name: String,
        type_params: Vec<TypeParam>,
        type_expr: TypeExpr,
        is_pub: bool,
    },
    SpawnExpr {
        body: Vec<SNode>,
    },
    /// Structured-concurrency nursery: `scope { ... }`. Tasks spawned while this
    /// block is on the task-scope stack are joined when the block exits — the
    /// first task error cancels its siblings and propagates out of the block, so
    /// no spawned task is orphaned or has its error silently swallowed.
    ScopeBlock {
        body: Vec<SNode>,
    },
    /// Duration literal: 500ms, 5s, 30m, 2h, 1d, 1w
    DurationLiteral(u64),
    /// Range expression: `start to end` (inclusive) or `start to end exclusive` (half-open)
    RangeExpr {
        start: Box<SNode>,
        end: Box<SNode>,
        inclusive: bool,
    },
    /// Guard clause: guard condition else { body }
    GuardStmt {
        condition: Box<SNode>,
        else_body: Vec<SNode>,
    },
    RequireStmt {
        condition: Box<SNode>,
        message: Option<Box<SNode>>,
    },
    /// Defer statement: defer { body } — runs body at scope exit.
    DeferStmt {
        body: Vec<SNode>,
    },
    /// Deadline block: deadline DURATION { body }
    DeadlineBlock {
        duration: Box<SNode>,
        body: Vec<SNode>,
    },
    /// Yield expression: yields control to host, optionally with a value.
    YieldExpr {
        value: Option<Box<SNode>>,
    },
    /// Emit expression: emits one value from a `gen fn` stream.
    EmitExpr {
        value: Box<SNode>,
    },
    /// Mutex block: mutual exclusion for concurrent access.
    ///
    /// `key` is the optional resource expression in `mutex(resource) { ... }`.
    /// When present, all blocks acquiring the same structural key value
    /// mutually exclude; when absent (`mutex { ... }`), the block keys on its
    /// own lexical call-site, so two distinct `mutex {}` blocks no longer
    /// serialize against each other.
    MutexBlock {
        key: Option<Box<SNode>>,
        body: Vec<SNode>,
    },
    /// Break out of a loop.
    BreakStmt,
    /// Continue to next loop iteration.
    ContinueStmt,

    /// First-class HITL primitive expression.
    ///
    /// Lexed as a reserved keyword (`request_approval`, `dual_control`,
    /// `ask_user`, `escalate_to`), parsed at primary-expression position
    /// as `keyword "(" args ")"`. Each arg is either positional
    /// (`expr`) or named (`name: expr`).
    ///
    /// The compiler lowers this to a call to the matching async stdlib
    /// builtin in `crates/harn-vm/src/stdlib/hitl.rs`, packaging the
    /// named arguments into the existing options-dict shape. The
    /// typechecker assigns each kind its canonical envelope return type.
    HitlExpr {
        kind: HitlKind,
        args: Vec<HitlArg>,
    },

    Parallel {
        mode: ParallelMode,
        /// For Count mode: the count expression. For Each/Settle: the list expression.
        expr: Box<SNode>,
        variable: Option<String>,
        body: Vec<SNode>,
        /// Optional trailing `with { max_concurrent: N, ... }` option block.
        /// A vec (rather than a dict) preserves source order for error
        /// reporting and keeps parsing cheap. Only `max_concurrent` is
        /// currently honored; unknown keys are rejected by the parser.
        options: Vec<(String, SNode)>,
    },

    SelectExpr {
        cases: Vec<SelectCase>,
        timeout: Option<(Box<SNode>, Vec<SNode>)>,
        default_body: Option<Vec<SNode>>,
    },

    FunctionCall {
        name: String,
        type_args: Vec<TypeExpr>,
        args: Vec<SNode>,
    },
    MethodCall {
        object: Box<SNode>,
        method: String,
        args: Vec<SNode>,
    },
    /// Optional method call: `obj?.method(args)` — returns nil if obj is nil.
    OptionalMethodCall {
        object: Box<SNode>,
        method: String,
        args: Vec<SNode>,
    },
    PropertyAccess {
        object: Box<SNode>,
        property: String,
    },
    /// Optional chaining: `obj?.property` — returns nil if obj is nil.
    OptionalPropertyAccess {
        object: Box<SNode>,
        property: String,
    },
    SubscriptAccess {
        object: Box<SNode>,
        index: Box<SNode>,
    },
    /// Optional subscript: `obj?.[index]` — returns nil if obj is nil.
    OptionalSubscriptAccess {
        object: Box<SNode>,
        index: Box<SNode>,
    },
    SliceAccess {
        object: Box<SNode>,
        start: Option<Box<SNode>>,
        end: Option<Box<SNode>>,
    },
    BinaryOp {
        op: String,
        left: Box<SNode>,
        right: Box<SNode>,
    },
    UnaryOp {
        op: String,
        operand: Box<SNode>,
    },
    Ternary {
        condition: Box<SNode>,
        true_expr: Box<SNode>,
        false_expr: Box<SNode>,
    },
    Assignment {
        target: Box<SNode>,
        value: Box<SNode>,
        /// None = plain `=`, Some("+") = `+=`, etc.
        op: Option<String>,
    },
    ThrowStmt {
        value: Box<SNode>,
    },

    /// Enum variant construction: EnumName.Variant(args)
    EnumConstruct {
        enum_name: String,
        variant: String,
        args: Vec<SNode>,
    },
    /// Struct construction: StructName { field: value, ... }
    StructConstruct {
        struct_name: String,
        fields: Vec<DictEntry>,
    },

    InterpolatedString(Vec<StringSegment>),
    StringLiteral(String),
    /// Raw string literal `r"..."` — no escape processing.
    RawStringLiteral(String),
    IntLiteral(i64),
    FloatLiteral(f64),
    BoolLiteral(bool),
    NilLiteral,
    Identifier(String),
    ListLiteral(Vec<SNode>),
    DictLiteral(Vec<DictEntry>),
    /// Spread expression `...expr` inside list/dict literals.
    Spread(Box<SNode>),
    /// Try operator: expr? — unwraps Result.Ok or propagates Result.Err.
    TryOperator {
        operand: Box<SNode>,
    },
    /// Non-null assertion: `expr!` — asserts the operand is not `nil`.
    /// Statically strips `nil` from the operand's type (`T | nil` -> `T`);
    /// at runtime it is identity when the value is present and throws a
    /// structured `unwrap_nil` error when it is `nil`.
    NonNullAssert {
        operand: Box<SNode>,
    },
    /// Try-star operator: `try* EXPR` — evaluates EXPR; on throw, runs
    /// pending finally blocks up to the enclosing catch and rethrows
    /// the original value. On success, evaluates to EXPR's value.
    /// Lowered per spec/HARN_SPEC.md as:
    ///   { let _r = try { EXPR }
    ///     guard is_ok(_r) else { throw unwrap_err(_r) }
    ///     unwrap(_r) }
    TryStar {
        operand: Box<SNode>,
    },

    /// Or-pattern in a `match` arm: `"ping" | "pong" -> body`. One or
    /// more alternative patterns that share a single arm body. Only
    /// legal inside a `MatchArm.pattern` slot.
    OrPattern(Vec<SNode>),

    Block(Vec<SNode>),
    Closure {
        params: Vec<TypedParam>,
        return_type: Option<TypeExpr>,
        /// Declared exception channel; see the [`Node::Pipeline`] `throws`
        /// field. `None` = unconstrained. Only the `fn(params) -> R throws E`
        /// closure spelling can carry it; the bare `x -> expr` arrow form has
        /// no place to put a clause and always parses `None`.
        throws: Option<TypeExpr>,
        body: Vec<SNode>,
        /// When true, this closure was written as `fn(params) { body }`.
        /// The formatter preserves this distinction.
        fn_syntax: bool,
    },
}

/// First-class human-in-the-loop primitive.
///
/// Each `HitlKind` is a reserved keyword expression with VM-enforced
/// semantics: the names cannot be shadowed or rebound by user code,
/// signatures are produced by the VM, and the audit log is recorded
/// deterministically by the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub enum HitlKind {
    /// `request_approval(action: ..., args: ..., quorum: ..., reviewers: ..., ...)`.
    RequestApproval,
    /// `dual_control(n: ..., m: ..., action: <closure>, approvers: ...)`.
    DualControl,
    /// `ask_user(prompt: ..., schema: ..., timeout: ..., default: ...)`.
    AskUser,
    /// `escalate_to(role: ..., reason: ...)`.
    EscalateTo,
}

impl HitlKind {
    /// Keyword surface form (matches the reserved keyword in the lexer
    /// and the corresponding async builtin name in the VM).
    pub fn as_keyword(self) -> &'static str {
        match self {
            HitlKind::RequestApproval => "request_approval",
            HitlKind::DualControl => "dual_control",
            HitlKind::AskUser => "ask_user",
            HitlKind::EscalateTo => "escalate_to",
        }
    }
}

/// A single argument in a [`Node::HitlExpr`] call. `name` is `Some` when
/// the caller used named-arg syntax (e.g. `quorum: 2`); positional
/// arguments leave it as `None` and rely on the kind's parameter order.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct HitlArg {
    pub name: Option<String>,
    pub value: SNode,
    pub span: Span,
}

/// Parallel execution mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ParallelMode {
    /// `parallel N { i -> ... }` — run N concurrent tasks.
    Count,
    /// `parallel each list { item -> ... }` — map over list concurrently.
    Each,
    /// `parallel each list { item -> ... } as stream` — emit as each task completes.
    EachStream,
    /// `parallel settle list { item -> ... }` — map with error collection.
    Settle,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MatchArm {
    pub pattern: SNode,
    /// Optional guard: `pattern if condition -> { body }`.
    pub guard: Option<Box<SNode>>,
    pub body: Vec<SNode>,
    /// Source extent of the whole arm, pattern through closing brace. The
    /// pattern's own span cannot bound the arm's body, so without this a
    /// comment after the arm's last statement has no range to be flushed in.
    /// See `StructField::span`.
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SelectCase {
    pub variable: String,
    pub channel: Box<SNode>,
    pub body: Vec<SNode>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DictEntry {
    pub key: SNode,
    pub value: SNode,
}

/// An enum variant declaration.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct EnumVariant {
    pub name: String,
    pub fields: Vec<TypedParam>,
    /// Source extent of the variant. A member without a span cannot anchor a
    /// comment written against it; see `StructField::span`.
    pub span: Span,
}

/// A struct field declaration.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct StructField {
    pub name: String,
    pub type_expr: Option<TypeExpr>,
    pub optional: bool,
    /// Source extent of the field.
    ///
    /// A comment's meaning lives entirely in where it sits, and a comment can
    /// only be placed next to something that knows its own source lines. Member
    /// items carrying no span is what let `harn fmt` evict a field's doc
    /// comment out of the struct and re-attach it to the next declaration,
    /// where it then described unrelated code.
    pub span: Span,
}

/// An associated-type entry in an interface body: `type Item` or
/// `type Item = string`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AssociatedType {
    pub name: String,
    /// The default, when written as `type Item = <default>`.
    pub default: Option<TypeExpr>,
    /// Source extent of the entry. See `StructField::span`.
    pub span: Span,
}

impl AssociatedType {
    /// The name/default pair, dropping source position — the shape the
    /// typechecker's semantic tables carry, which have no use for a span.
    pub fn to_binding(&self) -> (String, Option<TypeExpr>) {
        (self.name.clone(), self.default.clone())
    }

    /// Project a parsed interface body's associated types onto the binding
    /// pairs the semantic tables hold. The one place this conversion lives.
    pub fn bindings(items: &[AssociatedType]) -> Vec<(String, Option<TypeExpr>)> {
        items.iter().map(AssociatedType::to_binding).collect()
    }
}

/// An interface method signature.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct InterfaceMethod {
    pub name: String,
    pub type_params: Vec<TypeParam>,
    pub params: Vec<TypedParam>,
    pub return_type: Option<TypeExpr>,
    /// Source extent of the method signature. See `StructField::span`.
    pub span: Span,
}

/// A type annotation (optional, for runtime checking).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TypeExpr {
    /// A named type: int, string, float, bool, nil, list, dict, closure,
    /// or a user-defined type name.
    Named(String),
    /// A union type: `string | nil`, `int | float`.
    Union(Vec<TypeExpr>),
    /// An intersection type: `{x: int} & {y: int}`. The value must satisfy
    /// every component simultaneously. Useful for layered context types
    /// such as `fn use(ctx: BaseCtx & AuthCtx)`.
    Intersection(Vec<TypeExpr>),
    /// A dict shape type: `{name: string, age: int, active?: bool}`.
    Shape(Vec<ShapeField>),
    /// An **open** record / row-polymorphic shape: a set of explicit fields
    /// plus one or more trailing **row tails** (`{id: string, ...R}`,
    /// `{...R1, ...R2}`). Each tail in `rests` is a row variable
    /// (`Named(rowvar)`), a gradual map tail (`dict` / `dict<string, V>`), or a
    /// nested shape — folded left-to-right with right-biased merge once the row
    /// variables are bound. A closed shape stays `Shape` (empty `rests`).
    OpenShape {
        fields: Vec<ShapeField>,
        rests: Vec<TypeExpr>,
    },
    /// A list type: `list<int>`.
    List(Box<TypeExpr>),
    /// A dict type with key and value types: `dict<string, int>`.
    DictType(Box<TypeExpr>, Box<TypeExpr>),
    /// A lazy iterator type: `iter<int>`. Yields values of the inner type
    /// via the combinator/sink protocol (`VmValue::Iter` at runtime).
    Iter(Box<TypeExpr>),
    /// A synchronous generator type: `Generator<int>`. Produced by a regular
    /// `fn` body containing `yield`.
    Generator(Box<TypeExpr>),
    /// An asynchronous stream type: `Stream<int>`. Produced by `gen fn`.
    Stream(Box<TypeExpr>),
    /// An owned handle type: `owned<File>`. Marks the binding as carrying
    /// sole ownership of a drop-able resource. The compiler emits an
    /// auto-`drop()` at the binding's enclosing block exit; the lint
    /// `HARN-OWN-005` flags ownership leaks (e.g. returning the value or
    /// storing it in a non-owned field).
    Owned(Box<TypeExpr>),
    /// A generic type application: `Option<int>`, `Result<string, int>`.
    Applied { name: String, args: Vec<TypeExpr> },
    /// A function type: `fn(int, string) -> bool`.
    FnType {
        params: Vec<TypeExpr>,
        return_type: Box<TypeExpr>,
    },
    /// The bottom type: the type of expressions that never produce a value
    /// (return, throw, break, continue).
    Never,
    /// A string-literal type: `"pass"`, `"fail"`. Assignable to `string`.
    /// Used in unions to represent enum-like discriminated values.
    LitString(String),
    /// An int-literal type: `0`, `1`, `-1`. Assignable to `int`.
    LitInt(i64),
}

/// A field in a dict shape type.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ShapeField {
    pub name: String,
    pub type_expr: TypeExpr,
    pub optional: bool,
}

/// A binding pattern for destructuring in let/var/for-in.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum BindingPattern {
    /// Simple identifier: `let x = ...`
    Identifier(String),
    /// Dict destructuring: `let {name, age} = ...`
    Dict(Vec<DictPatternField>),
    /// List destructuring: `let [a, b] = ...`
    List(Vec<ListPatternElement>),
    /// Pair destructuring for `for (a, b) in iter { ... }`. The iter must
    /// yield `VmValue::Pair` values. Not valid in let/var bindings.
    Pair(String, String),
}

/// `_` is the discard binding name in `let`/`var`/destructuring positions.
pub fn is_discard_name(name: &str) -> bool {
    name == "_"
}

/// A field in a dict destructuring pattern.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DictPatternField {
    /// The dict key to extract.
    pub key: String,
    /// Renamed binding (if different from key), e.g. `{name: alias}`.
    pub alias: Option<String>,
    /// True for `...rest` (rest pattern).
    pub is_rest: bool,
    /// Default value if the key is missing (nil), e.g. `{name = "default"}`.
    pub default_value: Option<Box<SNode>>,
}

/// An element in a list destructuring pattern.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ListPatternElement {
    /// The variable name to bind.
    pub name: String,
    /// True for `...rest` (rest pattern).
    pub is_rest: bool,
    /// Default value if the index is out of bounds (nil), e.g. `[a = 0]`.
    pub default_value: Option<Box<SNode>>,
}

/// Declared variance of a generic type parameter.
///
/// - `Invariant` (default, no marker): the parameter appears in both
///   input and output positions, or mutable state. `T<A>` and `T<B>`
///   are unrelated unless `A == B`.
/// - `Covariant` (`out T`): the parameter appears only in output
///   positions (produced, not consumed). `T<Sub>` flows into
///   `T<Super>`.
/// - `Contravariant` (`in T`): the parameter appears only in input
///   positions (consumed, not produced). `T<Super>` flows into
///   `T<Sub>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Variance {
    Invariant,
    Covariant,
    Contravariant,
}

/// A generic type parameter on a function or pipeline declaration.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TypeParam {
    pub name: String,
    pub variance: Variance,
}

impl TypeParam {
    /// Construct an invariant type parameter (the default for
    /// unannotated `<T>`).
    pub fn invariant(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            variance: Variance::Invariant,
        }
    }
}

/// A where-clause constraint on a generic type parameter.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct WhereClause {
    pub type_name: String,
    pub bound: TypeExpr,
}

/// A parameter with an optional type annotation and optional default value.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TypedParam {
    pub name: String,
    pub type_expr: Option<TypeExpr>,
    pub default_value: Option<Box<SNode>>,
    /// If true, this is a rest parameter (`...name`) that collects remaining arguments.
    pub rest: bool,
}

impl TypedParam {
    /// Create an untyped parameter.
    pub fn untyped(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            type_expr: None,
            default_value: None,
            rest: false,
        }
    }

    /// Create a typed parameter.
    pub fn typed(name: impl Into<String>, type_expr: TypeExpr) -> Self {
        Self {
            name: name.into(),
            type_expr: Some(type_expr),
            default_value: None,
            rest: false,
        }
    }

    /// Extract just the names from a list of typed params.
    pub fn names(params: &[TypedParam]) -> Vec<String> {
        params.iter().map(|p| p.name.clone()).collect()
    }

    /// Return the index of the first parameter with a default value, or None.
    pub fn default_start(params: &[TypedParam]) -> Option<usize> {
        params.iter().position(|p| p.default_value.is_some())
    }
}
