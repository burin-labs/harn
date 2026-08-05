use super::ast::{BinOp, Expr, PathSeg, UnOp};
use super::error::TemplateError;
use crate::runtime_limits::RuntimeLimits;

const TEMPLATE_EXPR_MAX_DEPTH: usize = RuntimeLimits::DEFAULT.max_template_ast_depth;

pub(super) fn parse_expr(src: &str, line: usize, col: usize) -> Result<Expr, TemplateError> {
    let tokens = tokenize_expr(src, line, col)?;
    let mut p = ExprParser {
        toks: &tokens,
        pos: 0,
        line,
        col,
    };
    let e = p.parse_filter(0)?;
    if p.pos < tokens.len() {
        return Err(TemplateError::new(
            line,
            col,
            format!("unexpected token `{:?}` in expression", p.toks[p.pos]),
        ));
    }
    Ok(e)
}

#[derive(Debug, Clone, PartialEq)]
enum EToken {
    Ident(String),
    Str(String),
    Int(i64),
    Float(f64),
    LParen,
    RParen,
    LBracket,
    RBracket,
    Dot,
    Comma,
    Colon,
    Pipe,
    Bang,
    EqEq,
    BangEq,
    Lt,
    Le,
    Gt,
    Ge,
    AndKw,
    OrKw,
    NotKw,
    True,
    False,
    Nil,
}

fn tokenize_expr(src: &str, line: usize, col: usize) -> Result<Vec<EToken>, TemplateError> {
    let bytes = src.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match b {
            b'(' => {
                toks.push(EToken::LParen);
                i += 1;
            }
            b')' => {
                toks.push(EToken::RParen);
                i += 1;
            }
            b'[' => {
                toks.push(EToken::LBracket);
                i += 1;
            }
            b']' => {
                toks.push(EToken::RBracket);
                i += 1;
            }
            b'.' => {
                toks.push(EToken::Dot);
                i += 1;
            }
            b',' => {
                toks.push(EToken::Comma);
                i += 1;
            }
            b':' => {
                toks.push(EToken::Colon);
                i += 1;
            }
            b'|' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'|' {
                    toks.push(EToken::OrKw);
                    i += 2;
                } else {
                    toks.push(EToken::Pipe);
                    i += 1;
                }
            }
            b'&' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'&' {
                    toks.push(EToken::AndKw);
                    i += 2;
                } else {
                    return Err(TemplateError::new(line, col, "unexpected `&`"));
                }
            }
            b'!' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    toks.push(EToken::BangEq);
                    i += 2;
                } else {
                    toks.push(EToken::Bang);
                    i += 1;
                }
            }
            b'=' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    toks.push(EToken::EqEq);
                    i += 2;
                } else {
                    return Err(TemplateError::new(line, col, "unexpected `=` (use `==`)"));
                }
            }
            b'<' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    toks.push(EToken::Le);
                    i += 2;
                } else {
                    toks.push(EToken::Lt);
                    i += 1;
                }
            }
            b'>' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    toks.push(EToken::Ge);
                    i += 2;
                } else {
                    toks.push(EToken::Gt);
                    i += 1;
                }
            }
            b'"' | b'\'' => {
                let quote = b;
                let start = i + 1;
                let mut j = start;
                let mut out = String::new();
                while j < bytes.len() && bytes[j] != quote {
                    if bytes[j] == b'\\' && j + 1 < bytes.len() {
                        match bytes[j + 1] {
                            b'n' => out.push('\n'),
                            b't' => out.push('\t'),
                            b'r' => out.push('\r'),
                            b'\\' => out.push('\\'),
                            b'"' => out.push('"'),
                            b'\'' => out.push('\''),
                            c => out.push(c as char),
                        }
                        j += 2;
                        continue;
                    }
                    out.push(bytes[j] as char);
                    j += 1;
                }
                if j >= bytes.len() {
                    return Err(TemplateError::new(line, col, "unterminated string literal"));
                }
                toks.push(EToken::Str(out));
                i = j + 1;
            }
            b'0'..=b'9' | b'-'
                if b != b'-' || (i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit()) =>
            {
                let start = i;
                if bytes[i] == b'-' {
                    i += 1;
                }
                let mut is_float = false;
                while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                    if bytes[i] == b'.' {
                        if i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
                            is_float = true;
                            i += 1;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                let lex = &src[start..i];
                if is_float {
                    let v: f64 = lex.parse().map_err(|_| {
                        TemplateError::new(line, col, format!("invalid number `{lex}`"))
                    })?;
                    toks.push(EToken::Float(v));
                } else {
                    let v: i64 = lex.parse().map_err(|_| {
                        TemplateError::new(line, col, format!("invalid integer `{lex}`"))
                    })?;
                    toks.push(EToken::Int(v));
                }
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let word = &src[start..i];
                match word {
                    "true" => toks.push(EToken::True),
                    "false" => toks.push(EToken::False),
                    "nil" => toks.push(EToken::Nil),
                    "and" => toks.push(EToken::AndKw),
                    "or" => toks.push(EToken::OrKw),
                    "not" => toks.push(EToken::NotKw),
                    other => toks.push(EToken::Ident(other.to_string())),
                }
            }
            _ => {
                return Err(TemplateError::new(
                    line,
                    col,
                    format!("unexpected character `{}` in expression", b as char),
                ));
            }
        }
    }
    Ok(toks)
}

struct ExprParser<'a> {
    toks: &'a [EToken],
    pos: usize,
    line: usize,
    col: usize,
}

impl<'a> ExprParser<'a> {
    fn peek(&self) -> Option<&EToken> {
        self.toks.get(self.pos)
    }

    fn eat(&mut self, t: &EToken) -> bool {
        if self.peek() == Some(t) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn err(&self, m: impl Into<String>) -> TemplateError {
        TemplateError::new(self.line, self.col, m)
    }

    fn check_depth(&self, depth: usize) -> Result<(), TemplateError> {
        if depth > TEMPLATE_EXPR_MAX_DEPTH {
            Err(self.err(format!(
                "template expression depth exceeded ({TEMPLATE_EXPR_MAX_DEPTH} levels)"
            )))
        } else {
            Ok(())
        }
    }

    fn parse_filter(&mut self, depth: usize) -> Result<Expr, TemplateError> {
        self.check_depth(depth)?;
        let mut left = self.parse_or(depth)?;
        while self.eat(&EToken::Pipe) {
            let name = match self.peek() {
                Some(EToken::Ident(n)) => n.clone(),
                _ => return Err(self.err("expected filter name after `|`")),
            };
            self.pos += 1;
            let mut args = Vec::new();
            if self.eat(&EToken::Colon) {
                loop {
                    let a = self.parse_or(depth + 1)?;
                    args.push(a);
                    if !self.eat(&EToken::Comma) {
                        break;
                    }
                }
            }
            left = Expr::Filter(Box::new(left), name, args);
        }
        Ok(left)
    }

    fn parse_or(&mut self, depth: usize) -> Result<Expr, TemplateError> {
        self.check_depth(depth)?;
        let mut left = self.parse_and(depth)?;
        while self.eat(&EToken::OrKw) {
            let right = self.parse_and(depth)?;
            left = Expr::Binary(BinOp::Or, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self, depth: usize) -> Result<Expr, TemplateError> {
        self.check_depth(depth)?;
        let mut left = self.parse_not(depth)?;
        while self.eat(&EToken::AndKw) {
            let right = self.parse_not(depth)?;
            left = Expr::Binary(BinOp::And, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_not(&mut self, depth: usize) -> Result<Expr, TemplateError> {
        self.check_depth(depth)?;
        if self.eat(&EToken::Bang) || self.eat(&EToken::NotKw) {
            let inner = self.parse_not(depth + 1)?;
            return Ok(Expr::Unary(UnOp::Not, Box::new(inner)));
        }
        self.parse_cmp(depth)
    }

    fn parse_cmp(&mut self, depth: usize) -> Result<Expr, TemplateError> {
        self.check_depth(depth)?;
        let left = self.parse_unary(depth)?;
        let op = match self.peek() {
            Some(EToken::EqEq) => Some(BinOp::Eq),
            Some(EToken::BangEq) => Some(BinOp::Neq),
            Some(EToken::Lt) => Some(BinOp::Lt),
            Some(EToken::Le) => Some(BinOp::Le),
            Some(EToken::Gt) => Some(BinOp::Gt),
            Some(EToken::Ge) => Some(BinOp::Ge),
            _ => None,
        };
        if let Some(op) = op {
            self.pos += 1;
            let right = self.parse_unary(depth)?;
            return Ok(Expr::Binary(op, Box::new(left), Box::new(right)));
        }
        Ok(left)
    }

    fn parse_unary(&mut self, depth: usize) -> Result<Expr, TemplateError> {
        self.check_depth(depth)?;
        self.parse_primary(depth)
    }

    fn parse_primary(&mut self, depth: usize) -> Result<Expr, TemplateError> {
        self.check_depth(depth)?;
        let tok = self
            .peek()
            .cloned()
            .ok_or_else(|| self.err("expected expression"))?;
        self.pos += 1;
        let base = match tok {
            EToken::Nil => Expr::Nil,
            EToken::True => Expr::Bool(true),
            EToken::False => Expr::Bool(false),
            EToken::Int(n) => Expr::Int(n),
            EToken::Float(f) => Expr::Float(f),
            EToken::Str(s) => Expr::Str(s),
            EToken::LParen => {
                let e = self.parse_or(depth + 1)?;
                if !self.eat(&EToken::RParen) {
                    return Err(self.err("expected `)`"));
                }
                e
            }
            EToken::Ident(name) => self.parse_path(name)?,
            EToken::Bang | EToken::NotKw => {
                let inner = self.parse_primary(depth + 1)?;
                Expr::Unary(UnOp::Not, Box::new(inner))
            }
            other => return Err(self.err(format!("unexpected token `{other:?}`"))),
        };
        Ok(base)
    }

    fn parse_path(&mut self, head: String) -> Result<Expr, TemplateError> {
        let mut segs = vec![PathSeg::Field(head)];
        loop {
            match self.peek() {
                Some(EToken::Dot) => {
                    self.pos += 1;
                    match self.peek().cloned() {
                        Some(EToken::Ident(n)) => {
                            self.pos += 1;
                            segs.push(PathSeg::Field(n));
                        }
                        _ => return Err(self.err("expected identifier after `.`")),
                    }
                }
                Some(EToken::LBracket) => {
                    self.pos += 1;
                    match self.peek().cloned() {
                        Some(EToken::Int(n)) => {
                            self.pos += 1;
                            segs.push(PathSeg::Index(n));
                        }
                        Some(EToken::Str(s)) => {
                            self.pos += 1;
                            segs.push(PathSeg::Key(s));
                        }
                        _ => return Err(self.err("expected integer or string inside `[...]`")),
                    }
                    if !self.eat(&EToken::RBracket) {
                        return Err(self.err("expected `]`"));
                    }
                }
                _ => break,
            }
        }
        Ok(Expr::Path(segs))
    }
}
