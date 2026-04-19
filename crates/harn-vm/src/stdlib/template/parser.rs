use super::ast::{Expr, Node};
use super::error::TemplateError;
use super::expr_parser::parse_expr;
use super::lexer::{tokenize, Token};

pub(super) fn parse(src: &str) -> Result<Vec<Node>, TemplateError> {
    let tokens = tokenize(src)?;
    let mut p = Parser {
        tokens: &tokens,
        pos: 0,
    };
    let nodes = p.parse_block(&[])?;
    if p.pos < tokens.len() {
        // Unclosed block — shouldn't reach here; parse_block returns on EOF.
    }
    Ok(nodes)
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&'a Token> {
        self.tokens.get(self.pos)
    }

    fn parse_block(&mut self, stops: &[&str]) -> Result<Vec<Node>, TemplateError> {
        let mut out = Vec::new();
        while let Some(tok) = self.peek() {
            match tok {
                Token::Text {
                    content,
                    trim_right,
                    trim_left,
                } => {
                    let mut s = content.clone();
                    if *trim_left {
                        s = trim_leading_line(&s);
                    }
                    if *trim_right {
                        s = trim_trailing_line(&s);
                    }
                    if !s.is_empty() {
                        out.push(Node::Text(s));
                    }
                    self.pos += 1;
                }
                Token::Raw(content) => {
                    if !content.is_empty() {
                        out.push(Node::Text(content.clone()));
                    }
                    self.pos += 1;
                }
                Token::Directive { body, line, col } => {
                    let (line, col) = (*line, *col);
                    let body = body.clone();
                    let first_word = first_word(&body);
                    if stops.contains(&first_word) {
                        return Ok(out);
                    }
                    self.pos += 1;

                    if body == "end" {
                        return Err(TemplateError::new(line, col, "unexpected `{{ end }}`"));
                    }
                    if body == "else" {
                        return Err(TemplateError::new(line, col, "unexpected `{{ else }}`"));
                    }
                    if first_word == "elif" {
                        return Err(TemplateError::new(line, col, "unexpected `{{ elif }}`"));
                    }

                    if first_word == "if" {
                        let cond_src = body[2..].trim();
                        let cond = parse_expr(cond_src, line, col)?;
                        let node = self.parse_if(cond, line, col)?;
                        out.push(node);
                    } else if first_word == "for" {
                        let node = self.parse_for(body[3..].trim(), line, col)?;
                        out.push(node);
                    } else if first_word == "include" {
                        let node = parse_include(body[7..].trim(), line, col)?;
                        out.push(node);
                    } else if is_bare_ident(&body) {
                        out.push(Node::LegacyBareInterp { ident: body });
                    } else {
                        let expr = parse_expr(&body, line, col)?;
                        out.push(Node::Expr { expr, line, col });
                    }
                }
            }
        }
        Ok(out)
    }

    fn parse_if(
        &mut self,
        first_cond: Expr,
        line: usize,
        col: usize,
    ) -> Result<Node, TemplateError> {
        let mut branches = Vec::new();
        let mut else_branch = None;
        let mut cur_cond = first_cond;
        loop {
            let body = self.parse_block(&["end", "else", "elif"])?;
            branches.push((cur_cond, body));
            let tok = self.peek().cloned();
            match tok {
                Some(Token::Directive {
                    body: tbody,
                    line: tline,
                    col: tcol,
                }) => {
                    let fw = first_word(&tbody);
                    self.pos += 1;
                    match fw {
                        "end" => break,
                        "else" => {
                            let eb = self.parse_block(&["end"])?;
                            else_branch = Some(eb);
                            match self.peek() {
                                Some(Token::Directive { body, .. }) if body == "end" => {
                                    self.pos += 1;
                                }
                                _ => {
                                    return Err(TemplateError::new(
                                        tline,
                                        tcol,
                                        "`{{ else }}` missing matching `{{ end }}`",
                                    ));
                                }
                            }
                            break;
                        }
                        "elif" => {
                            let cond = parse_expr(tbody[4..].trim(), tline, tcol)?;
                            cur_cond = cond;
                            continue;
                        }
                        _ => unreachable!(),
                    }
                }
                _ => {
                    return Err(TemplateError::new(
                        line,
                        col,
                        "`{{ if }}` missing matching `{{ end }}`",
                    ));
                }
            }
        }
        Ok(Node::If {
            branches,
            else_branch,
            line,
            col,
        })
    }

    fn parse_for(&mut self, spec: &str, line: usize, col: usize) -> Result<Node, TemplateError> {
        let (head, iter_src) = match split_once_keyword(spec, " in ") {
            Some(p) => p,
            None => return Err(TemplateError::new(line, col, "expected `in` in for-loop")),
        };
        let head = head.trim();
        let iter_src = iter_src.trim();
        let (value_var, key_var) = if let Some((a, b)) = head.split_once(',') {
            let a = a.trim().to_string();
            let b = b.trim().to_string();
            if !is_ident(&a) || !is_ident(&b) {
                return Err(TemplateError::new(line, col, "invalid for-loop variables"));
            }
            (b, Some(a))
        } else {
            if !is_ident(head) {
                return Err(TemplateError::new(line, col, "invalid for-loop variable"));
            }
            (head.to_string(), None)
        };
        let iter = parse_expr(iter_src, line, col)?;
        let body = self.parse_block(&["end", "else"])?;
        let (empty, _) = match self.peek().cloned() {
            Some(Token::Directive { body: tbody, .. }) => {
                let fw = first_word(&tbody);
                self.pos += 1;
                if fw == "end" {
                    (None, ())
                } else if fw == "else" {
                    let empty_body = self.parse_block(&["end"])?;
                    match self.peek() {
                        Some(Token::Directive { body, .. }) if body == "end" => {
                            self.pos += 1;
                        }
                        _ => {
                            return Err(TemplateError::new(
                                line,
                                col,
                                "`{{ else }}` missing matching `{{ end }}`",
                            ));
                        }
                    }
                    (Some(empty_body), ())
                } else {
                    unreachable!()
                }
            }
            _ => {
                return Err(TemplateError::new(
                    line,
                    col,
                    "`{{ for }}` missing matching `{{ end }}`",
                ));
            }
        };
        Ok(Node::For {
            value_var,
            key_var,
            iter,
            body,
            empty,
            line,
            col,
        })
    }
}

fn parse_include(spec: &str, line: usize, col: usize) -> Result<Node, TemplateError> {
    let (path_src, with_src) = match split_once_keyword(spec, " with ") {
        Some((a, b)) => (a.trim(), Some(b.trim())),
        None => (spec.trim(), None),
    };
    let path = parse_expr(path_src, line, col)?;
    let with = if let Some(src) = with_src {
        Some(parse_dict_literal(src, line, col)?)
    } else {
        None
    };
    Ok(Node::Include {
        path,
        with,
        line,
        col,
    })
}

fn parse_dict_literal(
    src: &str,
    line: usize,
    col: usize,
) -> Result<Vec<(String, Expr)>, TemplateError> {
    let s = src.trim();
    if !s.starts_with('{') || !s.ends_with('}') {
        return Err(TemplateError::new(
            line,
            col,
            "expected `{ ... }` after `with`",
        ));
    }
    let inner = &s[1..s.len() - 1];
    let mut pairs = Vec::new();
    for chunk in split_top_level(inner, ',') {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        let (k, v) = match split_once_top_level(chunk, ':') {
            Some(p) => p,
            None => {
                return Err(TemplateError::new(
                    line,
                    col,
                    "expected `key: value` in include bindings",
                ));
            }
        };
        let k = k.trim();
        if !is_ident(k) {
            return Err(TemplateError::new(line, col, "invalid include binding key"));
        }
        let v = parse_expr(v.trim(), line, col)?;
        pairs.push((k.to_string(), v));
    }
    Ok(pairs)
}

fn first_word(s: &str) -> &str {
    s.split(|c: char| c.is_whitespace()).next().unwrap_or("")
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric() || c == '_')
}

fn is_bare_ident(s: &str) -> bool {
    is_ident(s)
}

fn trim_leading_line(s: &str) -> String {
    let mut i = 0;
    let bytes = s.as_bytes();
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'\n' {
        return s[i + 1..].to_string();
    }
    if i < bytes.len() && bytes[i] == b'\r' {
        if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
            return s[i + 2..].to_string();
        }
        return s[i + 1..].to_string();
    }
    s[i..].to_string()
}

fn trim_trailing_line(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut i = bytes.len();
    while i > 0 && (bytes[i - 1] == b' ' || bytes[i - 1] == b'\t') {
        i -= 1;
    }
    if i > 0 && bytes[i - 1] == b'\n' {
        let end = i - 1;
        let end = if end > 0 && bytes[end - 1] == b'\r' {
            end - 1
        } else {
            end
        };
        return s[..end].to_string();
    }
    s[..i].to_string()
}

fn split_top_level(s: &str, delim: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut quote = '"';
    let bytes = s.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i] as char;
        if in_str {
            if b == '\\' {
                i += 2;
                continue;
            }
            if b == quote {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match b {
            '"' | '\'' => {
                in_str = true;
                quote = b;
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            c if c == delim && depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&s[start..]);
    out
}

fn split_once_top_level(s: &str, delim: char) -> Option<(&str, &str)> {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut quote = '"';
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i] as char;
        if in_str {
            if b == '\\' {
                i += 2;
                continue;
            }
            if b == quote {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match b {
            '"' | '\'' => {
                in_str = true;
                quote = b;
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            c if c == delim && depth == 0 => {
                return Some((&s[..i], &s[i + 1..]));
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn split_once_keyword<'a>(s: &'a str, kw: &str) -> Option<(&'a str, &'a str)> {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut quote = '"';
    let bytes = s.as_bytes();
    let kw_bytes = kw.as_bytes();
    let mut i = 0;
    while i + kw_bytes.len() <= bytes.len() {
        let b = bytes[i] as char;
        if in_str {
            if b == '\\' {
                i += 2;
                continue;
            }
            if b == quote {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match b {
            '"' | '\'' => {
                in_str = true;
                quote = b;
                i += 1;
                continue;
            }
            '(' | '[' | '{' => {
                depth += 1;
                i += 1;
                continue;
            }
            ')' | ']' | '}' => {
                depth -= 1;
                i += 1;
                continue;
            }
            _ => {}
        }
        if depth == 0 && &bytes[i..i + kw_bytes.len()] == kw_bytes {
            return Some((&s[..i], &s[i + kw_bytes.len()..]));
        }
        i += 1;
    }
    None
}
