use harn_lexer::{Span, StringSegment};

/// A node wrapped with source location information.
#[derive(Debug, Clone, PartialEq)]
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

/// AST nodes for the Harn language.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    // Declarations
    Pipeline {
        name: String,
        params: Vec<String>,
        body: Vec<SNode>,
        extends: Option<String>,
    },
    LetBinding {
        pattern: BindingPattern,
        type_ann: Option<TypeExpr>,
        value: Box<SNode>,
    },
    VarBinding {
        pattern: BindingPattern,
        type_ann: Option<TypeExpr>,
        value: Box<SNode>,
    },
    OverrideDecl {
        name: String,
        params: Vec<String>,
        body: Vec<SNode>,
    },
    ImportDecl {
        path: String,
    },
    /// Selective import: import { foo, bar } from "module"
    SelectiveImport {
        names: Vec<String>,
        path: String,
    },
    EnumDecl {
        name: String,
        variants: Vec<EnumVariant>,
    },
    StructDecl {
        name: String,
        fields: Vec<StructField>,
    },
    InterfaceDecl {
        name: String,
        methods: Vec<InterfaceMethod>,
    },

    // Control flow
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
    ReturnStmt {
        value: Option<Box<SNode>>,
    },
    TryCatch {
        body: Vec<SNode>,
        error_var: Option<String>,
        error_type: Option<TypeExpr>,
        catch_body: Vec<SNode>,
    },
    FnDecl {
        name: String,
        params: Vec<TypedParam>,
        return_type: Option<TypeExpr>,
        body: Vec<SNode>,
        is_pub: bool,
    },
    TypeDecl {
        name: String,
        type_expr: TypeExpr,
    },
    SpawnExpr {
        body: Vec<SNode>,
    },
    /// Duration literal: 500ms, 5s, 30m, 2h
    DurationLiteral(u64),
    /// Range expression: start upto end (exclusive) or start thru end (inclusive)
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
    /// Ask expression: ask { system: "...", user: "...", ... }
    AskExpr {
        fields: Vec<DictEntry>,
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
    /// Mutex block: mutual exclusion for concurrent access.
    MutexBlock {
        body: Vec<SNode>,
    },
    /// Break out of a loop.
    BreakStmt,
    /// Continue to next loop iteration.
    ContinueStmt,

    // Concurrency
    Parallel {
        count: Box<SNode>,
        variable: Option<String>,
        body: Vec<SNode>,
    },
    ParallelMap {
        list: Box<SNode>,
        variable: String,
        body: Vec<SNode>,
    },

    // Expressions
    FunctionCall {
        name: String,
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

    // Literals
    InterpolatedString(Vec<StringSegment>),
    StringLiteral(String),
    IntLiteral(i64),
    FloatLiteral(f64),
    BoolLiteral(bool),
    NilLiteral,
    Identifier(String),
    ListLiteral(Vec<SNode>),
    DictLiteral(Vec<DictEntry>),
    /// Spread expression `...expr` inside list/dict literals.
    Spread(Box<SNode>),

    // Blocks
    Block(Vec<SNode>),
    Closure {
        params: Vec<TypedParam>,
        body: Vec<SNode>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pattern: SNode,
    pub body: Vec<SNode>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DictEntry {
    pub key: SNode,
    pub value: SNode,
}

/// An enum variant declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumVariant {
    pub name: String,
    pub fields: Vec<TypedParam>,
}

/// A struct field declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct StructField {
    pub name: String,
    pub type_expr: Option<TypeExpr>,
    pub optional: bool,
}

/// An interface method signature.
#[derive(Debug, Clone, PartialEq)]
pub struct InterfaceMethod {
    pub name: String,
    pub params: Vec<TypedParam>,
    pub return_type: Option<TypeExpr>,
}

/// A type annotation (optional, for runtime checking).
#[derive(Debug, Clone, PartialEq)]
pub enum TypeExpr {
    /// A named type: int, string, float, bool, nil, list, dict, closure,
    /// or a user-defined type name.
    Named(String),
    /// A union type: `string | nil`, `int | float`.
    Union(Vec<TypeExpr>),
    /// A dict shape type: `{name: string, age: int, active?: bool}`.
    Shape(Vec<ShapeField>),
    /// A list type: `list[int]`.
    List(Box<TypeExpr>),
    /// A dict type with key and value types: `dict[string, int]`.
    DictType(Box<TypeExpr>, Box<TypeExpr>),
}

/// A field in a dict shape type.
#[derive(Debug, Clone, PartialEq)]
pub struct ShapeField {
    pub name: String,
    pub type_expr: TypeExpr,
    pub optional: bool,
}

/// A binding pattern for destructuring in let/var/for-in.
#[derive(Debug, Clone, PartialEq)]
pub enum BindingPattern {
    /// Simple identifier: `let x = ...`
    Identifier(String),
    /// Dict destructuring: `let {name, age} = ...`
    Dict(Vec<DictPatternField>),
    /// List destructuring: `let [a, b] = ...`
    List(Vec<ListPatternElement>),
}

/// A field in a dict destructuring pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct DictPatternField {
    /// The dict key to extract.
    pub key: String,
    /// Renamed binding (if different from key), e.g. `{name: alias}`.
    pub alias: Option<String>,
    /// True for `...rest` (rest pattern).
    pub is_rest: bool,
}

/// An element in a list destructuring pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct ListPatternElement {
    /// The variable name to bind.
    pub name: String,
    /// True for `...rest` (rest pattern).
    pub is_rest: bool,
}

/// A parameter with an optional type annotation.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedParam {
    pub name: String,
    pub type_expr: Option<TypeExpr>,
}

impl TypedParam {
    /// Create an untyped parameter.
    pub fn untyped(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            type_expr: None,
        }
    }

    /// Create a typed parameter.
    pub fn typed(name: impl Into<String>, type_expr: TypeExpr) -> Self {
        Self {
            name: name.into(),
            type_expr: Some(type_expr),
        }
    }

    /// Extract just the names from a list of typed params.
    pub fn names(params: &[TypedParam]) -> Vec<String> {
        params.iter().map(|p| p.name.clone()).collect()
    }
}
