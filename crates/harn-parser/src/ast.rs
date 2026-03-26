use harn_lexer::StringSegment;

/// AST nodes for the Harn language.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    // Declarations
    Pipeline {
        name: String,
        params: Vec<String>,
        body: Vec<Node>,
        extends: Option<String>,
    },
    LetBinding {
        name: String,
        type_ann: Option<TypeExpr>,
        value: Box<Node>,
    },
    VarBinding {
        name: String,
        type_ann: Option<TypeExpr>,
        value: Box<Node>,
    },
    OverrideDecl {
        name: String,
        params: Vec<String>,
        body: Vec<Node>,
    },
    ImportDecl {
        path: String,
    },

    // Control flow
    IfElse {
        condition: Box<Node>,
        then_body: Vec<Node>,
        else_body: Option<Vec<Node>>,
    },
    ForIn {
        variable: String,
        iterable: Box<Node>,
        body: Vec<Node>,
    },
    MatchExpr {
        value: Box<Node>,
        arms: Vec<MatchArm>,
    },
    WhileLoop {
        condition: Box<Node>,
        body: Vec<Node>,
    },
    Retry {
        count: Box<Node>,
        body: Vec<Node>,
    },
    ReturnStmt {
        value: Option<Box<Node>>,
    },
    TryCatch {
        body: Vec<Node>,
        error_var: Option<String>,
        catch_body: Vec<Node>,
    },
    FnDecl {
        name: String,
        params: Vec<TypedParam>,
        return_type: Option<TypeExpr>,
        body: Vec<Node>,
    },
    TypeDecl {
        name: String,
        type_expr: TypeExpr,
    },
    SpawnExpr {
        body: Vec<Node>,
    },

    // Concurrency
    Parallel {
        count: Box<Node>,
        variable: Option<String>,
        body: Vec<Node>,
    },
    ParallelMap {
        list: Box<Node>,
        variable: String,
        body: Vec<Node>,
    },

    // Expressions
    FunctionCall {
        name: String,
        args: Vec<Node>,
    },
    MethodCall {
        object: Box<Node>,
        method: String,
        args: Vec<Node>,
    },
    PropertyAccess {
        object: Box<Node>,
        property: String,
    },
    SubscriptAccess {
        object: Box<Node>,
        index: Box<Node>,
    },
    BinaryOp {
        op: String,
        left: Box<Node>,
        right: Box<Node>,
    },
    UnaryOp {
        op: String,
        operand: Box<Node>,
    },
    Ternary {
        condition: Box<Node>,
        true_expr: Box<Node>,
        false_expr: Box<Node>,
    },
    Assignment {
        target: Box<Node>,
        value: Box<Node>,
    },
    ThrowStmt {
        value: Box<Node>,
    },

    // Literals
    InterpolatedString(Vec<StringSegment>),
    StringLiteral(String),
    IntLiteral(i64),
    FloatLiteral(f64),
    BoolLiteral(bool),
    NilLiteral,
    Identifier(String),
    ListLiteral(Vec<Node>),
    DictLiteral(Vec<DictEntry>),

    // Blocks
    Block(Vec<Node>),
    Closure {
        params: Vec<TypedParam>,
        body: Vec<Node>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pattern: Node,
    pub body: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DictEntry {
    pub key: Node,
    pub value: Node,
}

/// A type annotation (optional, for runtime checking).
#[derive(Debug, Clone, PartialEq)]
pub enum TypeExpr {
    /// A named type: int, string, float, bool, nil, list, dict, closure, or a user-defined type name.
    Named(String),
    /// A union type: `string | nil`, `int | float`.
    Union(Vec<TypeExpr>),
    /// A dict shape type: `{name: string, age: int, active?: bool}`.
    Shape(Vec<ShapeField>),
    /// A list type: `list[int]` (future extension).
    List(Box<TypeExpr>),
}

/// A field in a dict shape type.
#[derive(Debug, Clone, PartialEq)]
pub struct ShapeField {
    pub name: String,
    pub type_expr: TypeExpr,
    pub optional: bool,
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
