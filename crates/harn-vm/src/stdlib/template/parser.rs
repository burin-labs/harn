use super::ast::{Expr, IfBranch, Node};
use super::error::TemplateError;
use super::expr_parser::parse_expr;
use super::lexer::{tokenize, Token};
use super::outline::{OutlineBlock, OutlineBlockKind};
use super::sections;
use crate::runtime_limits::RuntimeLimits;

const TEMPLATE_AST_MAX_DEPTH: usize = RuntimeLimits::DEFAULT.max_template_ast_depth;

pub(super) fn parse(src: &str) -> Result<Vec<Node>, TemplateError> {
    Ok(parse_all(src)?.0)
}

/// Parse `src` for its block geometry alone. Shares the parser with
/// [`parse`] so an editor can never be shown a block structure the
/// engine wouldn't render.
pub(super) fn parse_outline(src: &str) -> Result<Vec<OutlineBlock>, TemplateError> {
    Ok(parse_all(src)?.1)
}

fn parse_all(src: &str) -> Result<(Vec<Node>, Vec<OutlineBlock>), TemplateError> {
    let tokens = tokenize(src)?;
    let mut p = Parser {
        tokens: &tokens,
        pos: 0,
        depth: 0,
        outline: Vec::new(),
    };
    let nodes = p.parse_block(&[])?;
    p.outline.sort_by_key(|block| (block.start, block.end));
    Ok((nodes, p.outline))
}

/// The `{{ .. }}` directive a block construct opens with: enough to
/// report an error against it and to anchor its outline range.
#[derive(Clone, Copy)]
struct DirectiveSite {
    line: usize,
    col: usize,
    start: usize,
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    depth: usize,
    outline: Vec<OutlineBlock>,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&'a Token> {
        self.tokens.get(self.pos)
    }

    fn push_block(&mut self, kind: OutlineBlockKind, start: usize, end: usize) {
        self.outline.push(OutlineBlock { kind, start, end });
    }

    fn parse_block(&mut self, stops: &[&str]) -> Result<Vec<Node>, TemplateError> {
        let (line, col) = self.peek_location();
        if self.depth >= TEMPLATE_AST_MAX_DEPTH {
            return Err(TemplateError::new(
                line,
                col,
                format!("template nesting depth exceeded ({TEMPLATE_AST_MAX_DEPTH} levels)"),
            ));
        }
        self.depth += 1;
        let result = self.parse_block_inner(stops);
        self.depth = self.depth.saturating_sub(1);
        result
    }

    fn parse_block_inner(&mut self, stops: &[&str]) -> Result<Vec<Node>, TemplateError> {
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
                Token::Raw {
                    content,
                    start,
                    end,
                } => {
                    if !content.is_empty() {
                        out.push(Node::Text(content.clone()));
                    }
                    self.push_block(OutlineBlockKind::Raw, *start, *end);
                    self.pos += 1;
                }
                Token::Comment { start, end } => {
                    self.push_block(OutlineBlockKind::Comment, *start, *end);
                    self.pos += 1;
                }
                Token::Directive {
                    body,
                    line,
                    col,
                    start,
                    ..
                } => {
                    let site = DirectiveSite {
                        line: *line,
                        col: *col,
                        start: *start,
                    };
                    let (line, col) = (site.line, site.col);
                    let body = body.clone();
                    let first_word = first_word(&body);
                    if stops.contains(&first_word) {
                        return Ok(out);
                    }
                    self.pos += 1;

                    if body == "end" {
                        return Err(TemplateError::new(line, col, "unexpected `{{ end }}`"));
                    }
                    if first_word == "endsection" {
                        return Err(TemplateError::new(
                            line,
                            col,
                            "unexpected `{{ endsection }}`",
                        ));
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
                        let node = self.parse_if(cond, site)?;
                        out.push(node);
                    } else if first_word == "for" {
                        let node = self.parse_for(body[3..].trim(), site)?;
                        out.push(node);
                    } else if first_word == "include" {
                        let node = parse_include(body[7..].trim(), line, col)?;
                        out.push(node);
                    } else if first_word == "section" {
                        let node = self.parse_section(body[7..].trim(), site)?;
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

    fn peek_location(&self) -> (usize, usize) {
        match self.peek() {
            Some(Token::Directive { line, col, .. }) => (*line, *col),
            _ => (1, 1),
        }
    }

    fn parse_if(&mut self, first_cond: Expr, site: DirectiveSite) -> Result<Node, TemplateError> {
        let DirectiveSite { line, col, .. } = site;
        let mut branches = Vec::new();
        let mut else_branch = None;
        let mut cur_cond = first_cond;
        let mut cur_line = line;
        let mut cur_col = col;
        // Every branch in the chain closes at the same `{{ end }}`, so
        // the outline ranges are backfilled once we reach it.
        let mut branch_starts = vec![(OutlineBlockKind::If, site.start)];
        let chain_end;
        loop {
            let body = self.parse_block(&["end", "else", "elif"])?;
            branches.push(IfBranch {
                cond: cur_cond,
                line: cur_line,
                col: cur_col,
                body,
            });
            let tok = self.peek().cloned();
            match tok {
                Some(Token::Directive {
                    body: tbody,
                    line: tline,
                    col: tcol,
                    start: tstart,
                    end: tend,
                }) => {
                    let fw = first_word(&tbody);
                    self.pos += 1;
                    match fw {
                        "end" => {
                            chain_end = tend;
                            break;
                        }
                        "else" => {
                            branch_starts.push((OutlineBlockKind::Else, tstart));
                            let eb = self.parse_block(&["end"])?;
                            else_branch = Some(eb);
                            match self.peek() {
                                Some(Token::Directive { body, end, .. }) if body == "end" => {
                                    chain_end = *end;
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
                            branch_starts.push((OutlineBlockKind::Elif, tstart));
                            cur_cond = cond;
                            cur_line = tline;
                            cur_col = tcol;
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
        for (kind, start) in branch_starts {
            self.push_block(kind, start, chain_end);
        }
        Ok(Node::If {
            branches,
            else_branch,
            line,
            col,
        })
    }

    fn parse_for(&mut self, spec: &str, site: DirectiveSite) -> Result<Node, TemplateError> {
        let DirectiveSite { line, col, .. } = site;
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
        let empty = match self.peek().cloned() {
            Some(Token::Directive {
                body: tbody,
                start: tstart,
                end: tend,
                ..
            }) => {
                let fw = first_word(&tbody);
                self.pos += 1;
                if fw == "end" {
                    self.push_block(OutlineBlockKind::For, site.start, tend);
                    None
                } else if fw == "else" {
                    let empty_body = self.parse_block(&["end"])?;
                    let loop_end = match self.peek() {
                        Some(Token::Directive { body, end, .. }) if body == "end" => {
                            self.pos += 1;
                            *end
                        }
                        _ => {
                            return Err(TemplateError::new(
                                line,
                                col,
                                "`{{ else }}` missing matching `{{ end }}`",
                            ));
                        }
                    };
                    self.push_block(OutlineBlockKind::For, site.start, loop_end);
                    self.push_block(OutlineBlockKind::Else, tstart, loop_end);
                    Some(empty_body)
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

    fn parse_section(&mut self, spec: &str, site: DirectiveSite) -> Result<Node, TemplateError> {
        let DirectiveSite { line, col, .. } = site;
        let (name, rest) = parse_section_name(spec, line, col)?;
        if !sections::is_builtin_section(&name) {
            return Err(TemplateError::new(
                line,
                col,
                format!("unknown template section `{name}`"),
            ));
        }
        let args = parse_section_args(rest, line, col)?;
        let body = self.parse_block(&["endsection"])?;
        match self.peek().cloned() {
            Some(Token::Directive {
                body: end,
                line: end_line,
                col: end_col,
                end: end_offset,
                ..
            }) if first_word(&end) == "endsection" => {
                self.pos += 1;
                if let Some(end_name) = parse_optional_endsection_name(&end, end_line, end_col)? {
                    if end_name != name {
                        return Err(TemplateError::new(
                            end_line,
                            end_col,
                            format!("mismatched section end: expected `{name}`, got `{end_name}`"),
                        ));
                    }
                }
                self.push_block(OutlineBlockKind::Section, site.start, end_offset);
            }
            _ => {
                return Err(TemplateError::new(
                    line,
                    col,
                    "`{{ section }}` missing matching `{{ endsection }}`",
                ));
            }
        }
        Ok(Node::Section {
            name,
            args,
            body,
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

fn parse_section_name(
    spec: &str,
    line: usize,
    col: usize,
) -> Result<(String, &str), TemplateError> {
    let s = spec.trim_start();
    let Some(quote) = s.as_bytes().first().copied() else {
        return Err(TemplateError::new(line, col, "expected section name"));
    };
    if quote != b'"' && quote != b'\'' {
        return Err(TemplateError::new(
            line,
            col,
            "section name must be a string literal",
        ));
    }
    let (name, consumed) = parse_quoted_literal(s, quote, line, col)?;
    Ok((name, &s[consumed..]))
}

fn parse_optional_endsection_name(
    body: &str,
    line: usize,
    col: usize,
) -> Result<Option<String>, TemplateError> {
    let rest = body["endsection".len()..].trim();
    if rest.is_empty() {
        return Ok(None);
    }
    let (name, tail) = parse_section_name(rest, line, col)?;
    if !tail.trim().is_empty() {
        return Err(TemplateError::new(
            line,
            col,
            "unexpected tokens after endsection name",
        ));
    }
    Ok(Some(name))
}

fn parse_quoted_literal(
    src: &str,
    quote: u8,
    line: usize,
    col: usize,
) -> Result<(String, usize), TemplateError> {
    let bytes = src.as_bytes();
    let mut out = String::new();
    let mut i = 1;
    while i < bytes.len() {
        let b = bytes[i];
        if b == quote {
            return Ok((out, i + 1));
        }
        if b == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'n' => out.push('\n'),
                b't' => out.push('\t'),
                b'r' => out.push('\r'),
                b'\\' => out.push('\\'),
                b'"' => out.push('"'),
                b'\'' => out.push('\''),
                c => out.push(c as char),
            }
            i += 2;
            continue;
        }
        out.push(b as char);
        i += 1;
    }
    Err(TemplateError::new(
        line,
        col,
        "unterminated section name string literal",
    ))
}

fn parse_section_args(
    src: &str,
    line: usize,
    col: usize,
) -> Result<Vec<(String, Expr)>, TemplateError> {
    let mut out = Vec::new();
    for chunk in split_section_arg_chunks(src) {
        let chunk = chunk.trim().trim_matches(',');
        if chunk.is_empty() {
            continue;
        }
        let (key, value) = split_once_top_level(chunk, '=').ok_or_else(|| {
            TemplateError::new(line, col, "expected `name=value` section argument")
        })?;
        let key = key.trim();
        if !is_ident(key) {
            return Err(TemplateError::new(
                line,
                col,
                "invalid section argument name",
            ));
        }
        out.push((key.to_string(), parse_expr(value.trim(), line, col)?));
    }
    Ok(out)
}

fn split_section_arg_chunks(s: &str) -> Vec<&str> {
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
            ',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            c if c.is_whitespace() && depth == 0 => {
                let mut j = i;
                while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                    j += 1;
                }
                if next_token_is_arg(&s[j..]) {
                    out.push(&s[start..i]);
                    start = j;
                }
                i = j;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&s[start..]);
    out
}

fn next_token_is_arg(s: &str) -> bool {
    let trimmed = s.trim_start();
    let Some(eq) = trimmed.find('=') else {
        return false;
    };
    let key = trimmed[..eq].trim();
    !key.is_empty() && is_ident(key)
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
