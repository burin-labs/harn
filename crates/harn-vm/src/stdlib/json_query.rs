use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{values_equal, VmValue};

pub(crate) fn eval_jq(input: &VmValue, source: &str) -> Result<Vec<VmValue>, String> {
    let mut parser = Parser::new(source);
    let expr = parser.parse_expr()?;
    parser.skip_ws();
    if !parser.is_eof() {
        return Err(format!(
            "jq: unexpected input near '{}'",
            parser.remaining()
        ));
    }
    Ok(expr.eval(input))
}

#[derive(Debug, Clone)]
enum Expr {
    Pipe(Vec<Expr>),
    Comma(Vec<Expr>),
    Path(Vec<PathStep>),
    RecursiveDescent,
    Builtin(Builtin),
    Map(Box<Expr>),
    Select(Box<Expr>),
    Object(Vec<(String, Expr)>),
    Literal(VmValue),
    Compare {
        left: Box<Expr>,
        op: CompareOp,
        right: Box<Expr>,
    },
    Bool {
        left: Box<Expr>,
        op: BoolOp,
        right: Box<Expr>,
    },
    Not(Box<Expr>),
}

#[derive(Debug, Clone)]
enum PathStep {
    Field(String),
    Iterate,
    Index(i64),
    Slice(Option<i64>, Option<i64>),
}

#[derive(Debug, Clone, Copy)]
enum Builtin {
    Length,
    Keys,
    Values,
    Type,
}

#[derive(Debug, Clone, Copy)]
enum CompareOp {
    Eq,
    Ne,
    Lt,
    Gt,
}

#[derive(Debug, Clone, Copy)]
enum BoolOp {
    And,
    Or,
}

impl Expr {
    fn eval(&self, input: &VmValue) -> Vec<VmValue> {
        match self {
            Expr::Pipe(stages) => {
                let mut stream = vec![input.clone()];
                for stage in stages {
                    let mut next = Vec::new();
                    for value in &stream {
                        next.extend(stage.eval(value));
                    }
                    stream = next;
                }
                stream
            }
            Expr::Comma(items) => items.iter().flat_map(|expr| expr.eval(input)).collect(),
            Expr::Path(steps) => eval_path(input, steps),
            Expr::RecursiveDescent => {
                let mut out = Vec::new();
                collect_recursive(input, &mut out);
                out
            }
            Expr::Builtin(builtin) => vec![eval_builtin(*builtin, input)],
            Expr::Map(expr) => match input {
                VmValue::List(items) => {
                    let mut mapped = Vec::new();
                    for item in items.iter() {
                        mapped.extend(expr.eval(item));
                    }
                    vec![VmValue::List(Rc::new(mapped))]
                }
                _ => vec![VmValue::List(Rc::new(Vec::new()))],
            },
            Expr::Select(expr) => {
                if expr.eval_bool(input) {
                    vec![input.clone()]
                } else {
                    Vec::new()
                }
            }
            Expr::Object(fields) => {
                let mut out = BTreeMap::new();
                for (key, expr) in fields {
                    out.insert(
                        key.clone(),
                        expr.eval(input).into_iter().next().unwrap_or(VmValue::Nil),
                    );
                }
                vec![VmValue::Dict(Rc::new(out))]
            }
            Expr::Literal(value) => vec![value.clone()],
            Expr::Compare { .. } | Expr::Bool { .. } | Expr::Not(_) => {
                vec![VmValue::Bool(self.eval_bool(input))]
            }
        }
    }

    fn eval_bool(&self, input: &VmValue) -> bool {
        match self {
            Expr::Compare { left, op, right } => {
                let left = left.eval(input).into_iter().next().unwrap_or(VmValue::Nil);
                let right = right.eval(input).into_iter().next().unwrap_or(VmValue::Nil);
                compare_values(&left, *op, &right)
            }
            Expr::Bool { left, op, right } => match op {
                BoolOp::And => left.eval_bool(input) && right.eval_bool(input),
                BoolOp::Or => left.eval_bool(input) || right.eval_bool(input),
            },
            Expr::Not(expr) => !expr.eval_bool(input),
            other => other
                .eval(input)
                .into_iter()
                .next()
                .is_some_and(|value| value.is_truthy()),
        }
    }
}

fn eval_path(input: &VmValue, steps: &[PathStep]) -> Vec<VmValue> {
    if steps.is_empty() {
        return vec![input.clone()];
    }

    let mut stream = vec![input.clone()];
    for step in steps {
        let mut next = Vec::new();
        for value in stream {
            match step {
                PathStep::Field(name) => match value {
                    VmValue::Dict(map) => {
                        next.push(map.get(name).cloned().unwrap_or(VmValue::Nil));
                    }
                    VmValue::StructInstance { .. } => {
                        next.push(value.struct_field(name).cloned().unwrap_or(VmValue::Nil));
                    }
                    _ => next.push(VmValue::Nil),
                },
                PathStep::Iterate => match value {
                    VmValue::List(items) | VmValue::Set(items) => {
                        next.extend(items.iter().cloned());
                    }
                    VmValue::Dict(map) => {
                        next.extend(map.values().cloned());
                    }
                    _ => {}
                },
                PathStep::Index(index) => match value {
                    VmValue::List(items) => {
                        next.push(index_list(&items, *index).unwrap_or(VmValue::Nil));
                    }
                    _ => next.push(VmValue::Nil),
                },
                PathStep::Slice(start, end) => match value {
                    VmValue::List(items) => {
                        next.push(VmValue::List(Rc::new(slice_list(&items, *start, *end))));
                    }
                    _ => next.push(VmValue::List(Rc::new(Vec::new()))),
                },
            }
        }
        stream = next;
    }
    stream
}

fn index_list(items: &[VmValue], index: i64) -> Option<VmValue> {
    let len = items.len() as i64;
    let resolved = if index < 0 { len + index } else { index };
    if resolved < 0 || resolved >= len {
        return None;
    }
    items.get(resolved as usize).cloned()
}

fn slice_list(items: &[VmValue], start: Option<i64>, end: Option<i64>) -> Vec<VmValue> {
    let len = items.len() as i64;
    let start = normalize_bound(start.unwrap_or(0), len);
    let end = normalize_bound(end.unwrap_or(len), len);
    if end <= start {
        return Vec::new();
    }
    items[start as usize..end as usize].to_vec()
}

fn normalize_bound(value: i64, len: i64) -> i64 {
    let resolved = if value < 0 { len + value } else { value };
    resolved.clamp(0, len)
}

fn collect_recursive(value: &VmValue, out: &mut Vec<VmValue>) {
    out.push(value.clone());
    match value {
        VmValue::List(items) | VmValue::Set(items) => {
            for item in items.iter() {
                collect_recursive(item, out);
            }
        }
        VmValue::Dict(map) => {
            for item in map.values() {
                collect_recursive(item, out);
            }
        }
        VmValue::StructInstance { .. } => {
            if let Some(fields) = value.struct_fields_map() {
                for item in fields.values() {
                    collect_recursive(item, out);
                }
            }
        }
        _ => {}
    }
}

fn eval_builtin(builtin: Builtin, input: &VmValue) -> VmValue {
    match builtin {
        Builtin::Length => VmValue::Int(match input {
            VmValue::String(s) => s.chars().count() as i64,
            VmValue::Bytes(bytes) => bytes.len() as i64,
            VmValue::List(items) | VmValue::Set(items) => items.len() as i64,
            VmValue::Dict(map) => map.len() as i64,
            VmValue::Nil => 0,
            _ => 1,
        }),
        Builtin::Keys => match input {
            VmValue::Dict(map) => VmValue::List(Rc::new(
                map.keys()
                    .map(|key| VmValue::String(Rc::from(key.as_str())))
                    .collect(),
            )),
            VmValue::List(items) => VmValue::List(Rc::new(
                (0..items.len())
                    .map(|index| VmValue::Int(index as i64))
                    .collect(),
            )),
            _ => VmValue::List(Rc::new(Vec::new())),
        },
        Builtin::Values => match input {
            VmValue::Dict(map) => VmValue::List(Rc::new(map.values().cloned().collect())),
            VmValue::List(items) | VmValue::Set(items) => VmValue::List(items.clone()),
            _ => VmValue::List(Rc::new(Vec::new())),
        },
        Builtin::Type => VmValue::String(Rc::from(jq_type_name(input))),
    }
}

fn jq_type_name(input: &VmValue) -> &'static str {
    match input {
        VmValue::Nil => "null",
        VmValue::Bool(_) => "boolean",
        VmValue::Int(_) | VmValue::Float(_) => "number",
        VmValue::String(_) => "string",
        VmValue::List(_) | VmValue::Set(_) => "array",
        VmValue::Dict(_) | VmValue::StructInstance { .. } => "object",
        _ => input.type_name(),
    }
}

fn compare_values(left: &VmValue, op: CompareOp, right: &VmValue) -> bool {
    match op {
        CompareOp::Eq => values_equal(left, right),
        CompareOp::Ne => !values_equal(left, right),
        CompareOp::Lt => ordering_cmp(left, right).is_some_and(|ord| ord.is_lt()),
        CompareOp::Gt => ordering_cmp(left, right).is_some_and(|ord| ord.is_gt()),
    }
}

fn ordering_cmp(left: &VmValue, right: &VmValue) -> Option<std::cmp::Ordering> {
    match (left, right) {
        (VmValue::Int(a), VmValue::Int(b)) => Some(a.cmp(b)),
        (VmValue::Float(a), VmValue::Float(b)) => a.partial_cmp(b),
        (VmValue::Int(a), VmValue::Float(b)) => (*a as f64).partial_cmp(b),
        (VmValue::Float(a), VmValue::Int(b)) => a.partial_cmp(&(*b as f64)),
        (VmValue::String(a), VmValue::String(b)) => Some(a.cmp(b)),
        _ => None,
    }
}

struct Parser<'a> {
    source: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Self {
        Self { source, pos: 0 }
    }

    fn parse_expr(&mut self) -> Result<Expr, String> {
        self.parse_pipe(true)
    }

    fn parse_pipe(&mut self, allow_comma: bool) -> Result<Expr, String> {
        let mut stages = vec![if allow_comma {
            self.parse_comma()?
        } else {
            self.parse_bool()?
        }];
        loop {
            self.skip_ws();
            if !self.consume_char('|') {
                break;
            }
            stages.push(if allow_comma {
                self.parse_comma()?
            } else {
                self.parse_bool()?
            });
        }
        Ok(if stages.len() == 1 {
            stages.remove(0)
        } else {
            Expr::Pipe(stages)
        })
    }

    fn parse_comma(&mut self) -> Result<Expr, String> {
        let mut items = vec![self.parse_bool()?];
        loop {
            self.skip_ws();
            if !self.consume_char(',') {
                break;
            }
            items.push(self.parse_bool()?);
        }
        Ok(if items.len() == 1 {
            items.remove(0)
        } else {
            Expr::Comma(items)
        })
    }

    fn parse_bool(&mut self) -> Result<Expr, String> {
        let mut expr = self.parse_compare()?;
        loop {
            if self.consume_keyword("and") {
                let right = self.parse_compare()?;
                expr = Expr::Bool {
                    left: Box::new(expr),
                    op: BoolOp::And,
                    right: Box::new(right),
                };
            } else if self.consume_keyword("or") {
                let right = self.parse_compare()?;
                expr = Expr::Bool {
                    left: Box::new(expr),
                    op: BoolOp::Or,
                    right: Box::new(right),
                };
            } else {
                break;
            }
        }
        Ok(expr)
    }

    fn parse_compare(&mut self) -> Result<Expr, String> {
        let left = self.parse_unary()?;
        self.skip_ws();
        let op = if self.consume_str("==") {
            Some(CompareOp::Eq)
        } else if self.consume_str("!=") {
            Some(CompareOp::Ne)
        } else if self.consume_char('<') {
            Some(CompareOp::Lt)
        } else if self.consume_char('>') {
            Some(CompareOp::Gt)
        } else {
            None
        };
        if let Some(op) = op {
            let right = self.parse_unary()?;
            Ok(Expr::Compare {
                left: Box::new(left),
                op,
                right: Box::new(right),
            })
        } else {
            Ok(left)
        }
    }

    fn parse_unary(&mut self) -> Result<Expr, String> {
        if self.consume_keyword("not") {
            return Ok(Expr::Not(Box::new(self.parse_unary()?)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        self.skip_ws();
        if self.consume_char('(') {
            let expr = self.parse_expr()?;
            self.expect_char(')')?;
            return Ok(expr);
        }
        if self.peek_str("..") {
            self.consume_str("..");
            return Ok(Expr::RecursiveDescent);
        }
        if self.peek_char() == Some('.') {
            return self.parse_path();
        }
        if self.peek_char() == Some('{') {
            return self.parse_object();
        }
        if self.peek_char() == Some('"') {
            return Ok(Expr::Literal(VmValue::String(Rc::from(
                self.parse_string()?.as_str(),
            ))));
        }
        if self
            .peek_char()
            .is_some_and(|ch| ch == '-' || ch.is_ascii_digit())
        {
            return self.parse_number();
        }
        let Some(ident) = self.try_parse_ident() else {
            return Err(format!(
                "jq: expected expression near '{}'",
                self.remaining()
            ));
        };
        match ident.as_str() {
            "true" => Ok(Expr::Literal(VmValue::Bool(true))),
            "false" => Ok(Expr::Literal(VmValue::Bool(false))),
            "null" | "nil" => Ok(Expr::Literal(VmValue::Nil)),
            "length" => Ok(Expr::Builtin(Builtin::Length)),
            "keys" => Ok(Expr::Builtin(Builtin::Keys)),
            "values" => Ok(Expr::Builtin(Builtin::Values)),
            "type" => Ok(Expr::Builtin(Builtin::Type)),
            "map" => {
                self.expect_char('(')?;
                let expr = self.parse_expr()?;
                self.expect_char(')')?;
                Ok(Expr::Map(Box::new(expr)))
            }
            "select" => {
                self.expect_char('(')?;
                let expr = self.parse_expr()?;
                self.expect_char(')')?;
                Ok(Expr::Select(Box::new(expr)))
            }
            _ => Err(format!("jq: unknown identifier '{ident}'")),
        }
    }

    fn parse_path(&mut self) -> Result<Expr, String> {
        self.expect_char('.')?;
        let mut steps = Vec::new();
        loop {
            self.skip_ws();
            if self.peek_char() == Some('[') {
                steps.push(self.parse_bracket_step()?);
                continue;
            }
            if self.consume_char('.') {
                self.skip_ws();
                if self.peek_char() == Some('[') {
                    steps.push(self.parse_bracket_step()?);
                    continue;
                }
                let Some(field) = self.try_parse_ident() else {
                    return Err(format!(
                        "jq: expected field name near '{}'",
                        self.remaining()
                    ));
                };
                steps.push(PathStep::Field(field));
                continue;
            }
            if let Some(field) = self.try_parse_ident() {
                steps.push(PathStep::Field(field));
                continue;
            }
            break;
        }
        Ok(Expr::Path(steps))
    }

    fn parse_bracket_step(&mut self) -> Result<PathStep, String> {
        self.expect_char('[')?;
        self.skip_ws();
        if self.consume_char(']') {
            return Ok(PathStep::Iterate);
        }
        if self.peek_char() == Some('"') {
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect_char(']')?;
            return Ok(PathStep::Field(key));
        }
        let start = self.try_parse_signed_int()?;
        self.skip_ws();
        if self.consume_char(':') {
            let end = self.try_parse_signed_int()?;
            self.skip_ws();
            self.expect_char(']')?;
            return Ok(PathStep::Slice(start, end));
        }
        self.expect_char(']')?;
        let Some(index) = start else {
            return Err("jq: expected array index or slice bound".to_string());
        };
        Ok(PathStep::Index(index))
    }

    fn parse_object(&mut self) -> Result<Expr, String> {
        self.expect_char('{')?;
        let mut fields = Vec::new();
        loop {
            self.skip_ws();
            if self.consume_char('}') {
                break;
            }
            let key = if self.peek_char() == Some('"') {
                self.parse_string()?
            } else {
                self.try_parse_ident()
                    .ok_or_else(|| format!("jq: expected object key near '{}'", self.remaining()))?
            };
            self.expect_char(':')?;
            let value = self.parse_pipe(false)?;
            fields.push((key, value));
            self.skip_ws();
            if self.consume_char('}') {
                break;
            }
            self.expect_char(',')?;
        }
        Ok(Expr::Object(fields))
    }

    fn parse_number(&mut self) -> Result<Expr, String> {
        let start = self.pos;
        self.consume_char_raw('-');
        self.consume_digits();
        let mut is_float = false;
        if self.consume_char_raw('.') {
            is_float = true;
            self.consume_digits();
        }
        if self.peek_char().is_some_and(|ch| ch == 'e' || ch == 'E') {
            is_float = true;
            self.pos += 1;
            let _ = self.consume_char_raw('+') || self.consume_char_raw('-');
            self.consume_digits();
        }
        let raw = &self.source[start..self.pos];
        if raw == "-" || raw.is_empty() {
            return Err(format!("jq: invalid number near '{}'", self.remaining()));
        }
        if is_float {
            raw.parse::<f64>()
                .map(|value| Expr::Literal(VmValue::Float(value)))
                .map_err(|error| format!("jq: invalid float '{raw}': {error}"))
        } else {
            raw.parse::<i64>()
                .map(|value| Expr::Literal(VmValue::Int(value)))
                .map_err(|error| format!("jq: invalid integer '{raw}': {error}"))
        }
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.skip_ws();
        if self.peek_char() != Some('"') {
            return Err(format!("jq: expected string near '{}'", self.remaining()));
        }
        let start = self.pos;
        self.pos += 1;
        let mut escaped = false;
        while let Some(ch) = self.peek_char() {
            self.pos += ch.len_utf8();
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                let raw = &self.source[start..self.pos];
                return serde_json::from_str::<String>(raw)
                    .map_err(|error| format!("jq: invalid string literal: {error}"));
            }
        }
        Err("jq: unterminated string literal".to_string())
    }

    fn try_parse_signed_int(&mut self) -> Result<Option<i64>, String> {
        self.skip_ws();
        let start = self.pos;
        self.consume_char('-');
        let before_digits = self.pos;
        self.consume_digits();
        if self.pos == before_digits {
            self.pos = start;
            return Ok(None);
        }
        self.source[start..self.pos]
            .parse::<i64>()
            .map(Some)
            .map_err(|error| format!("jq: invalid integer: {error}"))
    }

    fn try_parse_ident(&mut self) -> Option<String> {
        self.skip_ws();
        let mut chars = self.source[self.pos..].char_indices();
        let (_, first) = chars.next()?;
        if !(first == '_' || first.is_ascii_alphabetic()) {
            return None;
        }
        let mut end = self.pos + first.len_utf8();
        for (offset, ch) in chars {
            if ch == '_' || ch.is_ascii_alphanumeric() {
                end = self.pos + offset + ch.len_utf8();
            } else {
                break;
            }
        }
        let ident = self.source[self.pos..end].to_string();
        self.pos = end;
        Some(ident)
    }

    fn consume_digits(&mut self) {
        while self.peek_char().is_some_and(|ch| ch.is_ascii_digit()) {
            self.pos += 1;
        }
    }

    fn consume_keyword(&mut self, keyword: &str) -> bool {
        self.skip_ws();
        if !self.source[self.pos..].starts_with(keyword) {
            return false;
        }
        let end = self.pos + keyword.len();
        if self.source[end..]
            .chars()
            .next()
            .is_some_and(|ch| ch == '_' || ch.is_ascii_alphanumeric())
        {
            return false;
        }
        self.pos = end;
        true
    }

    fn expect_char(&mut self, expected: char) -> Result<(), String> {
        self.skip_ws();
        if self.consume_char(expected) {
            Ok(())
        } else {
            Err(format!(
                "jq: expected '{expected}' near '{}'",
                self.remaining()
            ))
        }
    }

    fn consume_char(&mut self, expected: char) -> bool {
        self.skip_ws();
        if self.peek_char() == Some(expected) {
            self.pos += expected.len_utf8();
            true
        } else {
            false
        }
    }

    fn consume_str(&mut self, expected: &str) -> bool {
        self.skip_ws();
        if self.source[self.pos..].starts_with(expected) {
            self.pos += expected.len();
            true
        } else {
            false
        }
    }

    fn consume_char_raw(&mut self, expected: char) -> bool {
        if self.peek_char() == Some(expected) {
            self.pos += expected.len_utf8();
            true
        } else {
            false
        }
    }

    fn peek_str(&self, expected: &str) -> bool {
        self.source[self.pos..].starts_with(expected)
    }

    fn peek_char(&self) -> Option<char> {
        self.source[self.pos..].chars().next()
    }

    fn skip_ws(&mut self) {
        while self.peek_char().is_some_and(char::is_whitespace) {
            self.pos += self.peek_char().unwrap().len_utf8();
        }
    }

    fn is_eof(&self) -> bool {
        self.pos >= self.source.len()
    }

    fn remaining(&self) -> &str {
        &self.source[self.pos..]
    }
}
