#[derive(Debug, Clone)]
pub(super) enum Node {
    Text(String),
    Expr {
        expr: Expr,
        line: usize,
        col: usize,
    },
    If {
        branches: Vec<(Expr, Vec<Node>)>,
        else_branch: Option<Vec<Node>>,
        line: usize,
        col: usize,
    },
    For {
        value_var: String,
        key_var: Option<String>,
        iter: Expr,
        body: Vec<Node>,
        empty: Option<Vec<Node>>,
        line: usize,
        col: usize,
    },
    Include {
        path: Expr,
        with: Option<Vec<(String, Expr)>>,
        line: usize,
        col: usize,
    },
    /// A legacy bare `{{ident}}` that should silently pass-through its source
    /// text on miss — preserves pre-v2 semantics for back-compat.
    LegacyBareInterp {
        ident: String,
    },
}

#[derive(Debug, Clone)]
pub(super) enum Expr {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Path(Vec<PathSeg>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Filter(Box<Expr>, String, Vec<Expr>),
}

#[derive(Debug, Clone)]
pub(super) enum PathSeg {
    Field(String),
    Index(i64),
    Key(String),
}

#[derive(Debug, Clone, Copy)]
pub(super) enum UnOp {
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum BinOp {
    Eq,
    Neq,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}
