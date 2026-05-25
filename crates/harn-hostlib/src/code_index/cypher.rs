//! Minimal Cypher executor over [`super::SymbolGraph`].
//!
//! Supported subset (intentionally narrow — see issue #2434):
//!
//! ```text
//! query     = MATCH pattern [, pattern]* [WHERE expr] RETURN proj [, proj]*
//! pattern   = '(' var ':' label '{' kv (',' kv)* '}' ')'
//!             [ ('-' | '<-') '[' ':' edge ('*' bounds)? ']' ('->' | '-') node ]*
//! bounds    = INT? '..' INT?
//! expr      = term [ ('AND' | 'OR') term ]*
//! term      = path op literal | path op path | literal
//! op        = '=' | '<>' | '!=' | '<' | '<=' | '>' | '>='
//! proj      = path [AS alias]
//! path      = ident '.' ident | ident
//! literal   = STRING | INT
//! ```
//!
//! Variable-length traversal up to depth 4 is supported via `*1..N` or
//! the shorthand `*` (defaults to `1..3`). When the upper bound is
//! omitted (e.g. `*1..` or `*2..`), it defaults to the executor depth
//! cap (4) — matching the usual Cypher "open upper bound = capped by
//! engine" semantics. The executor implements depth-first enumeration
//! with a per-step visited set so cycles can't infinite-loop.
//!
//! Read-only; no MERGE/CREATE/SET. The result is a flat
//! [`Vec<CypherRow>`] where each row maps the projected aliases to
//! [`CypherValue`]s.
//!
//! Execution is bounded by two budgets enforced inside [`exec`] and
//! [`parse`]:
//! * [`MAX_ROWS`] — maximum number of intermediate bindings *and*
//!   projected rows. The executor returns [`CypherError::ExecError`] as
//!   soon as it tries to push past the cap, so a degenerate
//!   `MATCH (a),(b),(c) RETURN ...` query cannot lock the symbol-graph
//!   index for arbitrarily long.
//! * [`MAX_PATTERNS`] — maximum number of comma-separated disjoint
//!   patterns in one query. Beyond this, the parser raises
//!   [`CypherError::ParseError`]; richer joins should use the edge
//!   syntax (`-[:CALLS]->`) which is bounded by [`MAX_ROWS`] but does
//!   not multiply pattern counts.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::rc::Rc;

use harn_vm::VmValue;

use super::symbol_graph::{EdgeKind, NodeId, NodeKind, SymbolGraph};

/// Hard upper bound on the number of rows the executor will materialize
/// before raising [`CypherError::ExecError`]. A query like
/// `MATCH (a),(b),(c),(d) RETURN ...` over a workspace symbol graph can
/// produce N⁴ rows in the worst case; without a cap a single Cypher call
/// can lock the host's symbol-graph index for many seconds. This budget
/// matches the schema description (`code_index/cypher.request.json`).
pub const MAX_ROWS: usize = 10_000;

/// Hard upper bound on the number of comma-separated `MATCH` patterns in
/// one query. Each additional pattern multiplies the row count, so we
/// refuse to even start enumerating beyond three disjoint patterns —
/// scripts that need a richer join should chain patterns through the
/// edge syntax (`-[:CALLS]->`) rather than relying on a cartesian
/// product. Enforced at parse time so the executor never sees one.
pub const MAX_PATTERNS: usize = 3;

/// Maximum traversal depth for variable-length patterns (`*lo..hi`).
/// Also used as the implicit upper bound when the high end is omitted
/// (e.g. `*1..`), matching standard "open upper bound" Cypher semantics.
const VAR_LENGTH_MAX_DEPTH: u32 = 4;

/// Error variants the parser/executor raise. The host wraps these in
/// [`crate::error::HostlibError`] before surfacing to scripts. Each
/// variant carries a free-text message identifying the offending token
/// or position.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum CypherError {
    /// Tokenizer reached an unexpected character.
    LexError(String),
    /// Parser saw a token it didn't recognize at this position.
    ParseError(String),
    /// Executor saw something semantically invalid (unknown variable,
    /// unknown edge label, var-length bound > 4, …).
    ExecError(String),
}

impl fmt::Display for CypherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CypherError::LexError(s) => write!(f, "lex error: {s}"),
            CypherError::ParseError(s) => write!(f, "parse error: {s}"),
            CypherError::ExecError(s) => write!(f, "exec error: {s}"),
        }
    }
}

impl std::error::Error for CypherError {}

/// One projected value in a row.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum CypherValue {
    /// SQL/Cypher `NULL`. Falls back to `VmValue::Nil`.
    Null,
    /// UTF-8 string.
    String(String),
    /// 64-bit signed integer.
    Int(i64),
    /// Boolean.
    Bool(bool),
}

impl CypherValue {
    /// Render as the corresponding [`VmValue`].
    pub fn to_vm(&self) -> VmValue {
        match self {
            CypherValue::Null => VmValue::Nil,
            CypherValue::String(s) => VmValue::String(Rc::from(s.as_str())),
            CypherValue::Int(n) => VmValue::Int(*n),
            CypherValue::Bool(b) => VmValue::Bool(*b),
        }
    }
}

/// One projected row.
pub type CypherRow = BTreeMap<String, CypherValue>;

/// Parse + execute `query` against `graph`. Returns one row per match.
pub fn execute(query: &str, graph: &SymbolGraph) -> Result<Vec<CypherRow>, CypherError> {
    let tokens = lex(query)?;
    let ast = parse(&tokens)?;
    exec(&ast, graph)
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Token {
    /// Keyword (uppercase form, case-insensitive matching at lex time).
    Keyword(String),
    Ident(String),
    Str(String),
    Int(i64),
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Colon,
    Comma,
    Dot,
    Eq,
    Neq,
    Lt,
    Le,
    Gt,
    Ge,
    Star,
    DotDot,
    Dash,
    Arrow,     // ->
    LeftArrow, // <-
}

fn lex(input: &str) -> Result<Vec<Token>, CypherError> {
    let mut out: Vec<Token> = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b == b'(' {
            out.push(Token::LParen);
            i += 1;
            continue;
        }
        if b == b')' {
            out.push(Token::RParen);
            i += 1;
            continue;
        }
        if b == b'{' {
            out.push(Token::LBrace);
            i += 1;
            continue;
        }
        if b == b'}' {
            out.push(Token::RBrace);
            i += 1;
            continue;
        }
        if b == b'[' {
            out.push(Token::LBracket);
            i += 1;
            continue;
        }
        if b == b']' {
            out.push(Token::RBracket);
            i += 1;
            continue;
        }
        if b == b':' {
            out.push(Token::Colon);
            i += 1;
            continue;
        }
        if b == b',' {
            out.push(Token::Comma);
            i += 1;
            continue;
        }
        if b == b'*' {
            out.push(Token::Star);
            i += 1;
            continue;
        }
        if b == b'=' {
            out.push(Token::Eq);
            i += 1;
            continue;
        }
        if b == b'.' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'.' {
                out.push(Token::DotDot);
                i += 2;
            } else {
                out.push(Token::Dot);
                i += 1;
            }
            continue;
        }
        if b == b'<' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'-' {
                out.push(Token::LeftArrow);
                i += 2;
            } else if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                out.push(Token::Le);
                i += 2;
            } else if i + 1 < bytes.len() && bytes[i + 1] == b'>' {
                out.push(Token::Neq);
                i += 2;
            } else {
                out.push(Token::Lt);
                i += 1;
            }
            continue;
        }
        if b == b'>' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                out.push(Token::Ge);
                i += 2;
            } else {
                out.push(Token::Gt);
                i += 1;
            }
            continue;
        }
        if b == b'!' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                out.push(Token::Neq);
                i += 2;
                continue;
            }
            return Err(CypherError::LexError(format!(
                "unexpected '!' at byte {i} (expected '!=')"
            )));
        }
        if b == b'-' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'>' {
                out.push(Token::Arrow);
                i += 2;
            } else {
                out.push(Token::Dash);
                i += 1;
            }
            continue;
        }
        if b == b'\'' || b == b'"' {
            let quote = b;
            let start = i + 1;
            i += 1;
            while i < bytes.len() && bytes[i] != quote {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if i >= bytes.len() {
                return Err(CypherError::LexError("unterminated string literal".into()));
            }
            let raw = &input[start..i];
            out.push(Token::Str(unescape(raw)));
            i += 1;
            continue;
        }
        if b.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let n: i64 = input[start..i].parse().map_err(|err| {
                CypherError::LexError(format!("bad integer at byte {start}: {err}"))
            })?;
            out.push(Token::Int(n));
            continue;
        }
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &input[start..i];
            let upper = word.to_ascii_uppercase();
            if is_keyword(&upper) {
                out.push(Token::Keyword(upper));
            } else {
                out.push(Token::Ident(word.to_string()));
            }
            continue;
        }
        return Err(CypherError::LexError(format!(
            "unexpected character `{}` at byte {i}",
            char::from(b)
        )));
    }
    Ok(out)
}

fn is_keyword(word: &str) -> bool {
    matches!(
        word,
        "MATCH" | "WHERE" | "RETURN" | "AND" | "OR" | "AS" | "NOT" | "TRUE" | "FALSE"
    )
}

fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut iter = raw.chars();
    while let Some(c) = iter.next() {
        if c == '\\' {
            if let Some(next) = iter.next() {
                out.push(match next {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    '\\' => '\\',
                    '\'' => '\'',
                    '"' => '"',
                    other => other,
                });
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Parser AST
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Query {
    matches: Vec<Pattern>,
    where_clause: Option<Expr>,
    projections: Vec<Projection>,
}

#[derive(Debug, Clone)]
struct Pattern {
    head: NodePat,
    steps: Vec<RelStep>,
}

#[derive(Debug, Clone)]
struct NodePat {
    var: String,
    label: Option<String>,
    props: BTreeMap<String, Literal>,
}

#[derive(Debug, Clone)]
struct RelStep {
    /// Direction the arrow points. true = forward (`-[]->`), false = reverse (`<-[]-`).
    forward: bool,
    edge_label: String,
    var_length: Option<(u32, u32)>,
    target: NodePat,
}

#[derive(Debug, Clone)]
enum Expr {
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Compare(Operand, CmpOp, Operand),
    Bool(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CmpOp {
    Eq,
    Neq,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone)]
enum Operand {
    /// `var.property` or just `var` (no property).
    Path {
        var: String,
        property: Option<String>,
    },
    Literal(Literal),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Literal {
    Str(String),
    Int(i64),
    Bool(bool),
}

#[derive(Debug, Clone)]
struct Projection {
    operand: Operand,
    /// Final alias used in the projected row. Empty during parsing
    /// when no explicit `AS` clause was supplied; filled in after the
    /// query is fully parsed by [`resolve_projection_aliases`].
    alias: String,
    /// `true` if the user supplied an `AS <name>` clause for this
    /// projection. Used to disambiguate default-alias collisions
    /// (e.g. `RETURN 1, 2` would otherwise project two `value` keys
    /// into the same `BTreeMap`).
    explicit_alias: bool,
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

fn parse(tokens: &[Token]) -> Result<Query, CypherError> {
    let mut p = Parser { tokens, pos: 0 };
    let q = p.parse_query()?;
    if p.pos != tokens.len() {
        return Err(CypherError::ParseError(format!(
            "trailing tokens after RETURN at position {} ({:?})",
            p.pos, tokens[p.pos]
        )));
    }
    Ok(q)
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> Option<&Token> {
        let t = self.tokens.get(self.pos);
        self.pos += 1;
        t
    }

    fn expect_keyword(&mut self, word: &str) -> Result<(), CypherError> {
        match self.advance() {
            Some(Token::Keyword(k)) if k == word => Ok(()),
            other => Err(CypherError::ParseError(format!(
                "expected `{word}`, got {other:?}"
            ))),
        }
    }

    fn match_keyword(&mut self, word: &str) -> bool {
        if matches!(self.peek(), Some(Token::Keyword(k)) if k == word) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, want: &Token) -> Result<(), CypherError> {
        let next = self.advance();
        if next == Some(want) {
            Ok(())
        } else {
            Err(CypherError::ParseError(format!(
                "expected {want:?}, got {next:?}"
            )))
        }
    }

    fn parse_query(&mut self) -> Result<Query, CypherError> {
        self.expect_keyword("MATCH")?;
        let mut matches = vec![self.parse_pattern()?];
        while matches!(self.peek(), Some(Token::Comma)) {
            self.advance();
            matches.push(self.parse_pattern()?);
            if matches.len() > MAX_PATTERNS {
                return Err(CypherError::ParseError(format!(
                    "too many disjoint MATCH patterns (got {}, cap is {}); \
                     join through edge syntax (`-[:KIND]->`) instead of a cartesian product",
                    matches.len(),
                    MAX_PATTERNS
                )));
            }
        }
        let where_clause = if self.match_keyword("WHERE") {
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect_keyword("RETURN")?;
        let mut projections = vec![self.parse_projection()?];
        while matches!(self.peek(), Some(Token::Comma)) {
            self.advance();
            projections.push(self.parse_projection()?);
        }
        resolve_projection_aliases(&mut projections)?;
        Ok(Query {
            matches,
            where_clause,
            projections,
        })
    }

    fn parse_pattern(&mut self) -> Result<Pattern, CypherError> {
        let head = self.parse_node_pat()?;
        let mut steps: Vec<RelStep> = Vec::new();
        loop {
            // Detect either `-[:X]->` or `<-[:X]-`.
            let forward = match self.peek() {
                Some(Token::Dash) => true,
                Some(Token::LeftArrow) => false,
                _ => break,
            };
            self.advance(); // consume - or <-
            self.expect(&Token::LBracket)?;
            self.expect(&Token::Colon)?;
            let edge_label = match self.advance() {
                Some(Token::Ident(s)) => s.clone(),
                other => {
                    return Err(CypherError::ParseError(format!(
                        "expected edge label, got {other:?}"
                    )))
                }
            };
            let var_length = if matches!(self.peek(), Some(Token::Star)) {
                self.advance();
                let lo = if matches!(self.peek(), Some(Token::Int(_))) {
                    if let Some(Token::Int(n)) = self.advance() {
                        Some(*n)
                    } else {
                        None
                    }
                } else {
                    None
                };
                let bounds = if matches!(self.peek(), Some(Token::DotDot)) {
                    self.advance();
                    let hi = if matches!(self.peek(), Some(Token::Int(_))) {
                        if let Some(Token::Int(n)) = self.advance() {
                            Some(*n)
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    // `*lo..` with no upper bound: default to the executor
                    // depth cap so users opt into the maximum traversal
                    // length without naming it explicitly.
                    let hi_v = hi.map(|n| n as u32).unwrap_or(VAR_LENGTH_MAX_DEPTH);
                    (lo.unwrap_or(1) as u32, hi_v)
                } else {
                    // bare `*`: 1..3
                    let lo_v = lo.unwrap_or(1) as u32;
                    (lo_v, lo_v.max(3))
                };
                Some(bounds)
            } else {
                None
            };
            self.expect(&Token::RBracket)?;
            if forward {
                self.expect(&Token::Arrow)?;
            } else {
                self.expect(&Token::Dash)?;
            }
            let target = self.parse_node_pat()?;
            steps.push(RelStep {
                forward,
                edge_label,
                var_length,
                target,
            });
        }
        Ok(Pattern { head, steps })
    }

    fn parse_node_pat(&mut self) -> Result<NodePat, CypherError> {
        self.expect(&Token::LParen)?;
        let var = match self.advance() {
            Some(Token::Ident(s)) => s.clone(),
            other => {
                return Err(CypherError::ParseError(format!(
                    "expected node variable, got {other:?}"
                )))
            }
        };
        let label = if matches!(self.peek(), Some(Token::Colon)) {
            self.advance();
            match self.advance() {
                Some(Token::Ident(s)) => Some(s.clone()),
                other => {
                    return Err(CypherError::ParseError(format!(
                        "expected node label, got {other:?}"
                    )))
                }
            }
        } else {
            None
        };
        let props = if matches!(self.peek(), Some(Token::LBrace)) {
            self.parse_props()?
        } else {
            BTreeMap::new()
        };
        self.expect(&Token::RParen)?;
        Ok(NodePat { var, label, props })
    }

    fn parse_props(&mut self) -> Result<BTreeMap<String, Literal>, CypherError> {
        self.expect(&Token::LBrace)?;
        let mut props: BTreeMap<String, Literal> = BTreeMap::new();
        loop {
            let key = match self.advance() {
                Some(Token::Ident(s)) => s.clone(),
                other => {
                    return Err(CypherError::ParseError(format!(
                        "expected property key, got {other:?}"
                    )))
                }
            };
            self.expect(&Token::Colon)?;
            let value = self.parse_literal()?;
            props.insert(key, value);
            match self.peek() {
                Some(Token::Comma) => {
                    self.advance();
                }
                Some(Token::RBrace) => break,
                other => {
                    return Err(CypherError::ParseError(format!(
                        "expected ',' or '}}', got {other:?}"
                    )))
                }
            }
        }
        self.expect(&Token::RBrace)?;
        Ok(props)
    }

    fn parse_literal(&mut self) -> Result<Literal, CypherError> {
        match self.advance() {
            Some(Token::Str(s)) => Ok(Literal::Str(s.clone())),
            Some(Token::Int(n)) => Ok(Literal::Int(*n)),
            Some(Token::Keyword(k)) if k == "TRUE" => Ok(Literal::Bool(true)),
            Some(Token::Keyword(k)) if k == "FALSE" => Ok(Literal::Bool(false)),
            other => Err(CypherError::ParseError(format!(
                "expected literal, got {other:?}"
            ))),
        }
    }

    fn parse_expr(&mut self) -> Result<Expr, CypherError> {
        let mut left = self.parse_and_expr()?;
        while self.match_keyword("OR") {
            let right = self.parse_and_expr()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and_expr(&mut self) -> Result<Expr, CypherError> {
        let mut left = self.parse_unary_expr()?;
        while self.match_keyword("AND") {
            let right = self.parse_unary_expr()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary_expr(&mut self) -> Result<Expr, CypherError> {
        if self.match_keyword("NOT") {
            let inner = self.parse_unary_expr()?;
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.parse_compare()
    }

    fn parse_compare(&mut self) -> Result<Expr, CypherError> {
        if matches!(
            self.peek(),
            Some(Token::Keyword(k)) if k == "TRUE" || k == "FALSE"
        ) {
            let lit = self.parse_literal()?;
            return Ok(match lit {
                Literal::Bool(b) => Expr::Bool(b),
                _ => unreachable!(),
            });
        }
        let left = self.parse_operand()?;
        let op = match self.peek() {
            Some(Token::Eq) => Some(CmpOp::Eq),
            Some(Token::Neq) => Some(CmpOp::Neq),
            Some(Token::Lt) => Some(CmpOp::Lt),
            Some(Token::Le) => Some(CmpOp::Le),
            Some(Token::Gt) => Some(CmpOp::Gt),
            Some(Token::Ge) => Some(CmpOp::Ge),
            _ => None,
        };
        let Some(op) = op else {
            return Err(CypherError::ParseError(
                "expected comparison operator in WHERE clause".into(),
            ));
        };
        self.advance();
        let right = self.parse_operand()?;
        Ok(Expr::Compare(left, op, right))
    }

    fn parse_operand(&mut self) -> Result<Operand, CypherError> {
        match self.peek() {
            Some(Token::Str(_)) | Some(Token::Int(_)) => {
                let lit = self.parse_literal()?;
                Ok(Operand::Literal(lit))
            }
            Some(Token::Keyword(k)) if k == "TRUE" || k == "FALSE" => {
                let lit = self.parse_literal()?;
                Ok(Operand::Literal(lit))
            }
            Some(Token::Ident(_)) => {
                let var = if let Some(Token::Ident(s)) = self.advance() {
                    s.clone()
                } else {
                    unreachable!()
                };
                let property = if matches!(self.peek(), Some(Token::Dot)) {
                    self.advance();
                    match self.advance() {
                        Some(Token::Ident(s)) => Some(s.clone()),
                        other => {
                            return Err(CypherError::ParseError(format!(
                                "expected property name after '.', got {other:?}"
                            )))
                        }
                    }
                } else {
                    None
                };
                Ok(Operand::Path { var, property })
            }
            other => Err(CypherError::ParseError(format!(
                "expected operand, got {other:?}"
            ))),
        }
    }

    fn parse_projection(&mut self) -> Result<Projection, CypherError> {
        let operand = self.parse_operand()?;
        let alias = if self.match_keyword("AS") {
            match self.advance() {
                Some(Token::Ident(s)) => Some(s.clone()),
                other => {
                    return Err(CypherError::ParseError(format!(
                        "expected alias after AS, got {other:?}"
                    )))
                }
            }
        } else {
            None
        };
        let explicit_alias = alias.is_some();
        Ok(Projection {
            operand,
            alias: alias.unwrap_or_default(),
            explicit_alias,
        })
    }
}

fn default_alias(operand: &Operand) -> String {
    match operand {
        Operand::Path {
            var,
            property: None,
        } => var.clone(),
        Operand::Path {
            var,
            property: Some(p),
        } => format!("{var}.{p}"),
        Operand::Literal(_) => "value".to_string(),
    }
}

/// Assign default aliases to every projection that didn't get an
/// explicit `AS <name>`. Default aliases for literal operands all start
/// as `"value"`, so without deduplication a query like `RETURN 1, 2`
/// would silently overwrite the first column. We resolve the collision
/// deterministically by suffixing the second, third, … occurrence with
/// `_2`, `_3`, … For non-literal default aliases (`var` or
/// `var.property`) we leave names unchanged because they are already
/// disambiguated by the underlying path. If a default-derived alias
/// collides with an explicit alias, the default is suffixed; an
/// explicit alias colliding with another explicit alias raises a
/// `ParseError::ParseError` because that is a user authoring mistake.
fn resolve_projection_aliases(projections: &mut [Projection]) -> Result<(), CypherError> {
    use std::collections::HashSet;

    // First pass: collect explicit aliases and detect duplicates among them.
    let mut seen: HashSet<String> = HashSet::new();
    for proj in projections.iter() {
        if proj.explicit_alias && !seen.insert(proj.alias.clone()) {
            return Err(CypherError::ParseError(format!(
                "duplicate alias `{}` in RETURN clause",
                proj.alias
            )));
        }
    }

    // Second pass: fill in default aliases with collision-suffixing.
    let mut literal_count: usize = 0;
    for proj in projections.iter_mut() {
        if proj.explicit_alias {
            continue;
        }
        let base = default_alias(&proj.operand);
        let is_literal_default = matches!(proj.operand, Operand::Literal(_));
        let mut candidate = base.clone();
        if is_literal_default {
            literal_count += 1;
            if literal_count >= 2 {
                candidate = format!("{base}_{literal_count}");
            }
        }
        // Guard against any other accidental collision (default path
        // alias clashing with an explicit alias or another default).
        let mut bump: usize = 2;
        while seen.contains(&candidate) {
            candidate = format!("{base}_{bump}");
            bump += 1;
        }
        seen.insert(candidate.clone());
        proj.alias = candidate;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Executor
// ---------------------------------------------------------------------------

fn exec(query: &Query, graph: &SymbolGraph) -> Result<Vec<CypherRow>, CypherError> {
    let mut all_rows: Vec<HashMap<String, NodeId>> = vec![HashMap::new()];
    for pattern in &query.matches {
        let mut new_rows: Vec<HashMap<String, NodeId>> = Vec::new();
        for binding in &all_rows {
            let candidates = match_pattern(pattern, graph, binding)?;
            for cand in candidates {
                let mut merged = binding.clone();
                for (k, v) in cand {
                    merged.insert(k, v);
                }
                new_rows.push(merged);
                // Pre-projection budget: a cartesian explosion can
                // balloon `new_rows` long before we ever start
                // building projected rows, so we cap intermediate
                // bindings too.
                if new_rows.len() > MAX_ROWS {
                    return Err(CypherError::ExecError(format!(
                        "row budget exceeded ({} > {}); narrow the MATCH or add a WHERE clause",
                        new_rows.len(),
                        MAX_ROWS
                    )));
                }
            }
        }
        all_rows = new_rows;
    }

    let mut out: Vec<CypherRow> = Vec::new();
    for binding in all_rows {
        if let Some(expr) = &query.where_clause {
            if !eval_bool(expr, &binding, graph)? {
                continue;
            }
        }
        let mut row: CypherRow = BTreeMap::new();
        for proj in &query.projections {
            let v = eval_operand(&proj.operand, &binding, graph)?;
            row.insert(proj.alias.clone(), v);
        }
        out.push(row);
        // Post-WHERE budget: even if intermediate bindings stayed
        // under the cap, a permissive projection on a wide graph can
        // still hand back too many rows.
        if out.len() > MAX_ROWS {
            return Err(CypherError::ExecError(format!(
                "row budget exceeded ({} > {}); narrow the MATCH or add a WHERE clause",
                out.len(),
                MAX_ROWS
            )));
        }
    }
    Ok(out)
}

fn match_pattern(
    pattern: &Pattern,
    graph: &SymbolGraph,
    seed: &HashMap<String, NodeId>,
) -> Result<Vec<HashMap<String, NodeId>>, CypherError> {
    let head_candidates: Vec<NodeId> = if let Some(existing) = seed.get(&pattern.head.var) {
        // Already bound — verify it matches the constraints.
        if node_matches(graph, *existing, &pattern.head) {
            vec![*existing]
        } else {
            vec![]
        }
    } else {
        candidate_nodes(graph, &pattern.head)
    };

    let mut out: Vec<HashMap<String, NodeId>> = Vec::new();
    for head_id in head_candidates {
        let mut binding: HashMap<String, NodeId> = HashMap::new();
        binding.insert(pattern.head.var.clone(), head_id);
        extend_steps(graph, &pattern.steps, 0, head_id, binding, &mut out)?;
    }
    Ok(out)
}

fn extend_steps(
    graph: &SymbolGraph,
    steps: &[RelStep],
    idx: usize,
    cursor: NodeId,
    binding: HashMap<String, NodeId>,
    out: &mut Vec<HashMap<String, NodeId>>,
) -> Result<(), CypherError> {
    if idx == steps.len() {
        out.push(binding);
        return Ok(());
    }
    let step = &steps[idx];
    let (kind, label_reversed) =
        EdgeKind::parse_with_direction(&step.edge_label).ok_or_else(|| {
            CypherError::ExecError(format!("unknown edge label `{}`", step.edge_label))
        })?;
    // Pattern arrow direction XOR label inversion = effective direction.
    let forward = step.forward ^ label_reversed;
    let (lo, hi) = step.var_length.unwrap_or((1, 1));
    if hi > VAR_LENGTH_MAX_DEPTH {
        return Err(CypherError::ExecError(format!(
            "variable-length traversal capped at depth {VAR_LENGTH_MAX_DEPTH} (got `*{lo}..{hi}`)"
        )));
    }
    let lo = lo.max(1);
    let hi = hi.max(lo);

    // Depth-first enumerate every reachable node at depths in [lo, hi].
    let mut visited: HashSet<NodeId> = HashSet::new();
    visited.insert(cursor);
    let mut stack: Vec<(NodeId, u32, HashSet<NodeId>)> = vec![(cursor, 0, visited)];
    while let Some((node, depth, visited)) = stack.pop() {
        if depth >= hi {
            continue;
        }
        let edges = if forward {
            graph.outgoing(node)
        } else {
            graph.incoming(node)
        };
        for edge in edges {
            if edge.kind != kind {
                continue;
            }
            let next = if forward { edge.to } else { edge.from };
            if visited.contains(&next) {
                continue;
            }
            let new_depth = depth + 1;
            if new_depth >= lo && node_matches(graph, next, &step.target) {
                let already_bound = binding.get(&step.target.var);
                if matches!(already_bound, Some(id) if *id != next) {
                    // Variable rebinds to a different id — skip.
                } else {
                    let mut next_binding = binding.clone();
                    next_binding.insert(step.target.var.clone(), next);
                    extend_steps(graph, steps, idx + 1, next, next_binding, out)?;
                }
            }
            if new_depth < hi {
                let mut next_visited = visited.clone();
                next_visited.insert(next);
                stack.push((next, new_depth, next_visited));
            }
        }
    }
    Ok(())
}

fn candidate_nodes(graph: &SymbolGraph, pat: &NodePat) -> Vec<NodeId> {
    // Property-driven fast path: if `name` is constrained, scan by name.
    if let Some(Literal::Str(name)) = pat.props.get("name") {
        return graph
            .nodes_named(name)
            .iter()
            .copied()
            .filter(|nid| node_matches(graph, *nid, pat))
            .collect();
    }
    if let Some(label) = &pat.label {
        let kind = NodeKind::parse(label).unwrap_or(NodeKind::Module);
        return graph
            .nodes_of_kind(kind)
            .into_iter()
            .filter(|nid| node_matches(graph, *nid, pat))
            .collect();
    }
    graph
        .all_node_ids()
        .into_iter()
        .filter(|nid| node_matches(graph, *nid, pat))
        .collect()
}

fn node_matches(graph: &SymbolGraph, id: NodeId, pat: &NodePat) -> bool {
    let Some(node) = graph.node(id) else {
        return false;
    };
    if let Some(label) = &pat.label {
        let Some(kind) = NodeKind::parse(label) else {
            return false;
        };
        if node.kind != kind {
            return false;
        }
    }
    for (key, expected) in &pat.props {
        let actual = property_value(node, key);
        if &actual != expected {
            return false;
        }
    }
    true
}

fn property_value(node: &super::symbol_graph::Node, key: &str) -> Literal {
    match key {
        "name" => Literal::Str(node.name.clone()),
        "path" => Literal::Str(node.path.clone()),
        "language" => Literal::Str(node.language.clone()),
        "kind" => Literal::Str(node.kind.as_str().to_string()),
        "container" => Literal::Str(node.container.clone().unwrap_or_default()),
        "signature" => Literal::Str(node.signature.clone()),
        "line" => Literal::Int(node.line as i64),
        "file_id" => Literal::Int(node.file_id as i64),
        "id" => Literal::Int(node.id as i64),
        _ => Literal::Str(String::new()),
    }
}

fn eval_bool(
    expr: &Expr,
    binding: &HashMap<String, NodeId>,
    graph: &SymbolGraph,
) -> Result<bool, CypherError> {
    match expr {
        Expr::Bool(b) => Ok(*b),
        Expr::And(a, b) => Ok(eval_bool(a, binding, graph)? && eval_bool(b, binding, graph)?),
        Expr::Or(a, b) => Ok(eval_bool(a, binding, graph)? || eval_bool(b, binding, graph)?),
        Expr::Not(inner) => Ok(!eval_bool(inner, binding, graph)?),
        Expr::Compare(l, op, r) => {
            let lv = lookup_operand(l, binding, graph)?;
            let rv = lookup_operand(r, binding, graph)?;
            Ok(apply_cmp(&lv, *op, &rv))
        }
    }
}

fn apply_cmp(left: &Literal, op: CmpOp, right: &Literal) -> bool {
    match (left, right) {
        (Literal::Int(a), Literal::Int(b)) => match op {
            CmpOp::Eq => a == b,
            CmpOp::Neq => a != b,
            CmpOp::Lt => a < b,
            CmpOp::Le => a <= b,
            CmpOp::Gt => a > b,
            CmpOp::Ge => a >= b,
        },
        (Literal::Str(a), Literal::Str(b)) => match op {
            CmpOp::Eq => a == b,
            CmpOp::Neq => a != b,
            CmpOp::Lt => a < b,
            CmpOp::Le => a <= b,
            CmpOp::Gt => a > b,
            CmpOp::Ge => a >= b,
        },
        (Literal::Bool(a), Literal::Bool(b)) => match op {
            CmpOp::Eq => a == b,
            CmpOp::Neq => a != b,
            _ => false,
        },
        _ => matches!(op, CmpOp::Neq),
    }
}

fn lookup_operand(
    op: &Operand,
    binding: &HashMap<String, NodeId>,
    graph: &SymbolGraph,
) -> Result<Literal, CypherError> {
    match op {
        Operand::Literal(lit) => Ok(lit.clone()),
        Operand::Path { var, property } => {
            let id = binding.get(var).copied().ok_or_else(|| {
                CypherError::ExecError(format!("unbound variable `{var}` in expression"))
            })?;
            let node = graph
                .node(id)
                .ok_or_else(|| CypherError::ExecError(format!("node id {id} not in graph")))?;
            match property {
                Some(p) => Ok(property_value(node, p)),
                None => Ok(Literal::Str(node.name.clone())),
            }
        }
    }
}

fn eval_operand(
    op: &Operand,
    binding: &HashMap<String, NodeId>,
    graph: &SymbolGraph,
) -> Result<CypherValue, CypherError> {
    let lit = lookup_operand(op, binding, graph)?;
    Ok(match lit {
        Literal::Str(s) => {
            if s.is_empty() {
                CypherValue::Null
            } else {
                CypherValue::String(s)
            }
        }
        Literal::Int(n) => CypherValue::Int(n),
        Literal::Bool(b) => CypherValue::Bool(b),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Language;

    fn fixture() -> SymbolGraph {
        let mut g = SymbolGraph::new();
        g.rebuild_file(
            1,
            "src/a.rs",
            Language::Rust,
            "fn start() {}\nfn driver() { start(); }\n",
            &[],
        );
        g.rebuild_file(
            2,
            "src/b.rs",
            Language::Rust,
            "fn entry() { driver(); }\n",
            &[],
        );
        g
    }

    #[test]
    fn lexer_recognises_arrows_and_keywords() {
        let toks = lex("MATCH (a)-[:CALLS]->(b) RETURN a.name").unwrap();
        assert!(toks.iter().any(|t| matches!(t, Token::Arrow)));
        assert!(toks
            .iter()
            .any(|t| matches!(t, Token::Keyword(k) if k == "MATCH")));
    }

    #[test]
    fn returns_function_by_name() {
        let g = fixture();
        let rows = execute(
            "MATCH (f:Function {name: 'start'}) RETURN f.path AS path, f.line AS line",
            &g,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get("path"),
            Some(&CypherValue::String("src/a.rs".into()))
        );
    }

    #[test]
    fn called_by_var_length_finds_indirect_callers() {
        let g = fixture();
        let rows = execute(
            "MATCH (f:Function {name: 'start'})<-[:CALLS*1..3]-(c:CallSite) RETURN c.path AS path",
            &g,
        )
        .unwrap();
        assert!(!rows.is_empty(), "expected at least one call-site caller");
    }

    #[test]
    fn where_predicate_filters_results() {
        let g = fixture();
        let rows = execute(
            "MATCH (f:Function) WHERE f.name = 'driver' RETURN f.path AS path",
            &g,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get("path"),
            Some(&CypherValue::String("src/a.rs".into()))
        );
    }

    #[test]
    fn literal_default_aliases_do_not_collide() {
        // `RETURN 1, 2, 3` previously projected all three literals under
        // the same `"value"` key, silently overwriting earlier columns.
        // The deduplicator suffixes the 2nd/3rd literals as `value_2`
        // / `value_3` so every literal makes it into the row.
        let g = fixture();
        let rows = execute("MATCH (f:Function {name: 'start'}) RETURN 1, 2, 3", &g).unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.get("value"), Some(&CypherValue::Int(1)));
        assert_eq!(row.get("value_2"), Some(&CypherValue::Int(2)));
        assert_eq!(row.get("value_3"), Some(&CypherValue::Int(3)));
    }

    #[test]
    fn duplicate_explicit_aliases_are_rejected() {
        let g = fixture();
        let err = execute("MATCH (f:Function) RETURN f.name AS n, f.path AS n", &g).unwrap_err();
        assert!(
            matches!(err, CypherError::ParseError(_)),
            "expected ParseError for duplicate explicit alias, got {err:?}"
        );
    }

    #[test]
    fn rejects_too_many_disjoint_patterns() {
        // Four disjoint patterns is one more than `MAX_PATTERNS`; the
        // parser refuses before the executor ever sees the cartesian
        // explosion.
        let g = fixture();
        let err = execute(
            "MATCH (a:Function),(b:Function),(c:Function),(d:Function) RETURN a.name",
            &g,
        )
        .unwrap_err();
        assert!(
            matches!(&err, CypherError::ParseError(msg) if msg.contains("too many disjoint MATCH patterns")),
            "expected ParseError for too many patterns, got {err:?}"
        );
    }

    #[test]
    fn enforces_row_budget_on_cartesian_explosion() {
        // Build a synthetic graph with enough Function nodes that even
        // a three-pattern cartesian product blows the row budget.
        // 25 * 25 * 25 = 15,625 > MAX_ROWS (10,000).
        let mut g = SymbolGraph::new();
        let mut source = String::new();
        for i in 0..25 {
            source.push_str(&format!("fn f{i}() {{}}\n"));
        }
        g.rebuild_file(1, "src/big.rs", Language::Rust, &source, &[]);

        let err = execute(
            "MATCH (a:Function),(b:Function),(c:Function) RETURN a.name AS n",
            &g,
        )
        .unwrap_err();
        assert!(
            matches!(&err, CypherError::ExecError(msg) if msg.contains("row budget exceeded")),
            "expected ExecError for row budget, got {err:?}"
        );
    }

    #[test]
    fn open_upper_bound_defaults_to_depth_cap() {
        // `*1..` (no hi) should fall back to VAR_LENGTH_MAX_DEPTH, not 3.
        let toks = lex("MATCH (a:Function)-[:CALLS*1..]->(b:Function) RETURN a.name AS n").unwrap();
        let q = parse(&toks).unwrap();
        let step = &q.matches[0].steps[0];
        assert_eq!(step.var_length, Some((1, VAR_LENGTH_MAX_DEPTH)));

        // `*2..` likewise.
        let toks = lex("MATCH (a:Function)-[:CALLS*2..]->(b:Function) RETURN a.name AS n").unwrap();
        let q = parse(&toks).unwrap();
        let step = &q.matches[0].steps[0];
        assert_eq!(step.var_length, Some((2, VAR_LENGTH_MAX_DEPTH)));
    }

    #[test]
    fn rejects_depth_above_four() {
        let g = fixture();
        let err = execute(
            "MATCH (a:Function)-[:CALLS*1..6]->(b:Function) RETURN a.name AS n",
            &g,
        )
        .unwrap_err();
        assert!(
            matches!(err, CypherError::ExecError(_)),
            "expected ExecError, got {err:?}"
        );
    }
}
