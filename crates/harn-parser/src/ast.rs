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
        value: Box<Node>,
    },
    VarBinding {
        name: String,
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
        params: Vec<String>,
        body: Vec<Node>,
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
        params: Vec<String>,
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
