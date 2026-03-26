use std::collections::BTreeMap;

use harn_lexer::Lexer;
use harn_parser::{Node, Parser};
use wasm_bindgen::prelude::*;

/// Execute Harn source code and return the output as a plain string.
///
/// This is a sync-only interpreter suitable for WASM environments.
/// Async features (spawn, parallel, http_*, llm_call) are not available.
#[wasm_bindgen]
pub fn run(source: &str) -> String {
    let mut lexer = Lexer::new(source);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(e) => return format!("Lexer error: {e}"),
    };

    let mut parser = Parser::new(tokens);
    let program = match parser.parse() {
        Ok(p) => p,
        Err(e) => return format!("Parse error: {e}"),
    };

    let mut interp = SyncInterpreter::new();
    match interp.run(&program) {
        Ok(()) => interp.output,
        Err(e) => format!("Runtime error: {e}"),
    }
}

/// Execute Harn source code and return the output (Result variant for JS interop).
///
/// This is a sync-only interpreter suitable for WASM environments.
/// Async features (spawn, parallel, http_*, llm_call) are not available.
#[wasm_bindgen]
pub fn execute(source: &str) -> Result<String, JsError> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().map_err(|e| JsError::new(&e.to_string()))?;

    let mut parser = Parser::new(tokens);
    let program = parser.parse().map_err(|e| JsError::new(&e.to_string()))?;

    let mut interp = SyncInterpreter::new();
    interp.run(&program).map_err(|e| JsError::new(&e))?;
    Ok(interp.output)
}

/// Lex source code and return tokens as JSON.
#[wasm_bindgen]
pub fn tokenize(source: &str) -> Result<String, JsError> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().map_err(|e| JsError::new(&e.to_string()))?;
    let token_strs: Vec<String> = tokens.iter().map(|t| format!("{}", t.kind)).collect();
    Ok(serde_json::to_string(&token_strs).unwrap_or_default())
}

/// Parse source code and return "ok" or the error message.
#[wasm_bindgen]
pub fn check(source: &str) -> String {
    let mut lexer = Lexer::new(source);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(e) => return e.to_string(),
    };
    let mut parser = Parser::new(tokens);
    match parser.parse() {
        Ok(_) => "ok".to_string(),
        Err(e) => e.to_string(),
    }
}

/// Format Harn source code.
#[wasm_bindgen]
pub fn format_code(source: &str) -> String {
    match harn_fmt::format_source(source) {
        Ok(formatted) => formatted,
        Err(e) => format!("Format error: {e}"),
    }
}

// --- Minimal sync interpreter for WASM ---
// This is a stripped-down version of the async Interpreter, without tokio dependencies.

use harn_lexer::StringSegment;
use harn_parser::{DictEntry, MatchArm};

struct SyncInterpreter {
    env: Env,
    pipelines: BTreeMap<String, Node>,
    output: String,
}

#[derive(Clone)]
enum Val {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Nil,
    List(Vec<Val>),
    Dict(BTreeMap<String, Val>),
    Closure(Vec<String>, Vec<Node>, Env),
}

impl Val {
    fn is_truthy(&self) -> bool {
        match self {
            Val::Bool(b) => *b,
            Val::Nil => false,
            Val::Int(n) => *n != 0,
            Val::Float(n) => *n != 0.0,
            Val::String(s) => !s.is_empty(),
            Val::List(l) => !l.is_empty(),
            Val::Dict(d) => !d.is_empty(),
            Val::Closure(..) => true,
        }
    }

    fn as_string(&self) -> String {
        match self {
            Val::String(s) => s.clone(),
            Val::Int(n) => n.to_string(),
            Val::Float(n) => {
                if *n == (*n as i64) as f64 && n.abs() < 1e15 {
                    format!("{:.1}", n)
                } else {
                    n.to_string()
                }
            }
            Val::Bool(b) => (if *b { "true" } else { "false" }).to_string(),
            Val::Nil => "nil".to_string(),
            Val::List(items) => {
                let inner: Vec<String> = items.iter().map(|i| i.as_string()).collect();
                format!("[{}]", inner.join(", "))
            }
            Val::Dict(map) => {
                let inner: Vec<String> =
                    map.iter().map(|(k, v)| format!("{k}: {}", v.as_string())).collect();
                format!("{{{}}}", inner.join(", "))
            }
            Val::Closure(..) => "<fn>".to_string(),
        }
    }

    fn as_int(&self) -> Option<i64> {
        if let Val::Int(n) = self { Some(*n) } else { None }
    }
}

fn vals_equal(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::String(x), Val::String(y)) => x == y,
        (Val::Int(x), Val::Int(y)) => x == y,
        (Val::Float(x), Val::Float(y)) => x == y,
        (Val::Bool(x), Val::Bool(y)) => x == y,
        (Val::Nil, Val::Nil) => true,
        (Val::Int(x), Val::Float(y)) => (*x as f64) == *y,
        (Val::Float(x), Val::Int(y)) => *x == (*y as f64),
        _ => false,
    }
}

#[derive(Clone)]
struct Env {
    values: BTreeMap<String, (Val, bool)>, // (value, mutable)
    parent: Option<Box<Env>>,
}

impl Env {
    fn new() -> Self {
        Self { values: BTreeMap::new(), parent: None }
    }

    fn child(&self) -> Self {
        Self { values: BTreeMap::new(), parent: Some(Box::new(self.clone())) }
    }

    fn get(&self, name: &str) -> Option<Val> {
        self.values.get(name).map(|(v, _)| v.clone())
            .or_else(|| self.parent.as_ref()?.get(name))
    }

    fn define(&mut self, name: &str, value: Val, mutable: bool) {
        self.values.insert(name.to_string(), (value, mutable));
    }

    fn assign(&mut self, name: &str, value: Val) -> Result<(), String> {
        if let Some((_, mutable)) = self.values.get(name) {
            if !mutable {
                return Err(format!("Cannot assign to immutable binding: {name}"));
            }
            self.values.insert(name.to_string(), (value, true));
            return Ok(());
        }
        if let Some(ref mut parent) = self.parent {
            parent.assign(name, value)
        } else {
            Err(format!("Undefined variable: {name}"))
        }
    }
}

impl SyncInterpreter {
    fn new() -> Self {
        Self {
            env: Env::new(),
            pipelines: BTreeMap::new(),
            output: String::new(),
        }
    }

    fn run(&mut self, program: &[Node]) -> Result<(), String> {
        for node in program {
            if let Node::Pipeline { name, .. } = node {
                self.pipelines.insert(name.clone(), node.clone());
            }
        }

        let main = self.pipelines.get("default").cloned().or_else(|| {
            program.iter().find(|n| matches!(n, Node::Pipeline { .. })).cloned()
        });

        let Some(Node::Pipeline { params, body, .. }) = main else { return Ok(()) };

        if params.iter().any(|p| p == "task") {
            self.env.define("task", Val::String(String::new()), false);
        }

        match self.exec(&body) {
            Ok(_) | Err(EvalStop::Return(_)) => Ok(()),
            Err(EvalStop::Error(e)) => Err(e),
        }
    }

    fn exec(&mut self, stmts: &[Node]) -> Result<Val, EvalStop> {
        let mut result = Val::Nil;
        for stmt in stmts {
            result = self.eval(stmt)?;
        }
        Ok(result)
    }

    fn eval(&mut self, node: &Node) -> Result<Val, EvalStop> {
        match node {
            Node::LetBinding { name, value } => {
                let val = self.eval(value)?;
                self.env.define(name, val, false);
                Ok(Val::Nil)
            }
            Node::VarBinding { name, value } => {
                let val = self.eval(value)?;
                self.env.define(name, val, true);
                Ok(Val::Nil)
            }
            Node::Assignment { target, value } => {
                let val = self.eval(value)?;
                if let Node::Identifier(name) = target.as_ref() {
                    self.env.assign(name, val).map_err(EvalStop::Error)?;
                }
                Ok(Val::Nil)
            }
            Node::IfElse { condition, then_body, else_body } => {
                if self.eval(condition)?.is_truthy() {
                    self.exec(then_body)
                } else if let Some(eb) = else_body {
                    self.exec(eb)
                } else {
                    Ok(Val::Nil)
                }
            }
            Node::ForIn { variable, iterable, body } => {
                let iter_val = self.eval(iterable)?;
                let items = match iter_val {
                    Val::List(items) => items,
                    Val::Dict(map) => map.into_iter().map(|(k, v)| {
                        Val::Dict(BTreeMap::from([
                            ("key".to_string(), Val::String(k)),
                            ("value".to_string(), v),
                        ]))
                    }).collect(),
                    _ => return Ok(Val::Nil),
                };
                let saved = self.env.clone();
                self.env = self.env.child();
                let mut result = Val::Nil;
                for item in items {
                    self.env.define(variable, item, true);
                    result = self.exec(body)?;
                }
                self.env = saved;
                Ok(result)
            }
            Node::WhileLoop { condition, body } => {
                let mut result = Val::Nil;
                for _ in 0..10_000 {
                    if !self.eval(condition)?.is_truthy() { break; }
                    result = self.exec(body)?;
                }
                Ok(result)
            }
            Node::ReturnStmt { value } => {
                let val = value.as_ref().map(|v| self.eval(v)).transpose()?;
                Err(EvalStop::Return(val.unwrap_or(Val::Nil)))
            }
            Node::ThrowStmt { value } => {
                let val = self.eval(value)?;
                Err(EvalStop::Error(format!("Thrown: {}", val.as_string())))
            }
            Node::TryCatch { body, error_var, catch_body } => {
                match self.exec(body) {
                    Ok(v) => Ok(v),
                    Err(EvalStop::Return(v)) => Err(EvalStop::Return(v)),
                    Err(EvalStop::Error(e)) => {
                        if let Some(var) = error_var {
                            self.env.define(var, Val::String(e), false);
                        }
                        self.exec(catch_body)
                    }
                }
            }
            Node::FnDecl { name, params, body } => {
                let closure = Val::Closure(params.clone(), body.clone(), self.env.clone());
                self.env.define(name, closure, false);
                Ok(Val::Nil)
            }
            Node::FunctionCall { name, args } => {
                if let Some(Val::Closure(params, body, cenv)) = self.env.get(name) {
                    let arg_vals: Result<Vec<_>, _> = args.iter().map(|a| self.eval(a)).collect();
                    return self.invoke_closure(&params, &body, &cenv, &arg_vals?);
                }
                if name == "log" {
                    let arg_vals: Result<Vec<_>, _> = args.iter().map(|a| self.eval(a)).collect();
                    let msg = arg_vals?.first().map(|v| v.as_string()).unwrap_or_default();
                    self.output.push_str(&format!("[harn] {msg}\n"));
                    return Ok(Val::Nil);
                }
                Err(EvalStop::Error(format!("Undefined builtin: {name}")))
            }
            Node::MethodCall { object, method, args } => {
                let obj = self.eval(object)?;
                let arg_vals: Result<Vec<_>, _> = args.iter().map(|a| self.eval(a)).collect();
                let arg_vals = arg_vals?;
                self.eval_method(obj, method, &arg_vals)
            }
            Node::PropertyAccess { object, property } => {
                let obj = self.eval(object)?;
                match &obj {
                    Val::Dict(map) => Ok(map.get(property).cloned().unwrap_or(Val::Nil)),
                    Val::List(items) => match property.as_str() {
                        "count" => Ok(Val::Int(items.len() as i64)),
                        "first" => Ok(items.first().cloned().unwrap_or(Val::Nil)),
                        "last" => Ok(items.last().cloned().unwrap_or(Val::Nil)),
                        "empty" => Ok(Val::Bool(items.is_empty())),
                        _ => Ok(Val::Nil),
                    },
                    Val::String(s) => match property.as_str() {
                        "count" => Ok(Val::Int(s.chars().count() as i64)),
                        "empty" => Ok(Val::Bool(s.is_empty())),
                        _ => Ok(Val::Nil),
                    },
                    _ => Ok(Val::Nil),
                }
            }
            Node::SubscriptAccess { object, index } => {
                let obj = self.eval(object)?;
                let idx = self.eval(index)?;
                match (&obj, &idx) {
                    (Val::List(items), Val::Int(i)) if *i >= 0 => {
                        Ok(items.get(*i as usize).cloned().unwrap_or(Val::Nil))
                    }
                    (Val::Dict(map), _) => {
                        Ok(map.get(&idx.as_string()).cloned().unwrap_or(Val::Nil))
                    }
                    _ => Ok(Val::Nil),
                }
            }
            Node::BinaryOp { op, left, right } => self.eval_binary(op, left, right),
            Node::UnaryOp { op, operand } => {
                let val = self.eval(operand)?;
                match op.as_str() {
                    "!" => Ok(Val::Bool(!val.is_truthy())),
                    "-" => match val {
                        Val::Int(n) => Ok(Val::Int(-n)),
                        Val::Float(n) => Ok(Val::Float(-n)),
                        _ => Ok(Val::Nil),
                    },
                    _ => Ok(Val::Nil),
                }
            }
            Node::Ternary { condition, true_expr, false_expr } => {
                if self.eval(condition)?.is_truthy() {
                    self.eval(true_expr)
                } else {
                    self.eval(false_expr)
                }
            }
            Node::InterpolatedString(segments) => {
                let mut result = String::new();
                for seg in segments {
                    match seg {
                        StringSegment::Literal(s) => result.push_str(s),
                        StringSegment::Expression(expr) => {
                            let mut lexer = Lexer::new(expr);
                            let tokens = lexer.tokenize().map_err(|e| EvalStop::Error(e.to_string()))?;
                            let mut parser = Parser::new(tokens);
                            let node = parser.parse_single_expression().map_err(|e| EvalStop::Error(e.to_string()))?;
                            let val = self.eval(&node)?;
                            result.push_str(&val.as_string());
                        }
                    }
                }
                Ok(Val::String(result))
            }
            Node::StringLiteral(s) => Ok(Val::String(s.clone())),
            Node::IntLiteral(n) => Ok(Val::Int(*n)),
            Node::FloatLiteral(n) => Ok(Val::Float(*n)),
            Node::BoolLiteral(b) => Ok(Val::Bool(*b)),
            Node::NilLiteral => Ok(Val::Nil),
            Node::Identifier(name) => Ok(self.env.get(name).unwrap_or(Val::Nil)),
            Node::ListLiteral(elements) => {
                let vals: Result<Vec<_>, _> = elements.iter().map(|e| self.eval(e)).collect();
                Ok(Val::List(vals?))
            }
            Node::DictLiteral(entries) => {
                let mut map = BTreeMap::new();
                for entry in entries {
                    let key = self.eval(&entry.key)?;
                    let val = self.eval(&entry.value)?;
                    map.insert(key.as_string(), val);
                }
                Ok(Val::Dict(map))
            }
            Node::Closure { params, body } => {
                Ok(Val::Closure(params.clone(), body.clone(), self.env.clone()))
            }
            Node::MatchExpr { value, arms } => {
                let val = self.eval(value)?;
                for arm in arms {
                    let pattern = self.eval(&arm.pattern)?;
                    if vals_equal(&val, &pattern) {
                        return self.exec(&arm.body);
                    }
                }
                Ok(Val::Nil)
            }
            _ => Ok(Val::Nil),
        }
    }

    fn invoke_closure(&mut self, params: &[String], body: &[Node], cenv: &Env, args: &[Val]) -> Result<Val, EvalStop> {
        let saved = self.env.clone();
        self.env = cenv.child();
        for (i, param) in params.iter().enumerate() {
            self.env.define(param, args.get(i).cloned().unwrap_or(Val::Nil), false);
        }
        let result = self.exec(body);
        self.env = saved;
        match result {
            Ok(v) => Ok(v),
            Err(EvalStop::Return(v)) => Ok(v),
            Err(e) => Err(e),
        }
    }

    fn eval_method(&mut self, obj: Val, method: &str, args: &[Val]) -> Result<Val, EvalStop> {
        match &obj {
            Val::String(s) => match method {
                "contains" => Ok(Val::Bool(s.contains(&args.first().map(|a| a.as_string()).unwrap_or_default()))),
                "replace" if args.len() >= 2 => Ok(Val::String(s.replace(&args[0].as_string(), &args[1].as_string()))),
                "split" => {
                    let sep = args.first().map(|a| a.as_string()).unwrap_or(",".into());
                    Ok(Val::List(s.split(&sep).map(|p| Val::String(p.to_string())).collect()))
                }
                "trim" => Ok(Val::String(s.trim().to_string())),
                "starts_with" => Ok(Val::Bool(s.starts_with(&args.first().map(|a| a.as_string()).unwrap_or_default()))),
                "ends_with" => Ok(Val::Bool(s.ends_with(&args.first().map(|a| a.as_string()).unwrap_or_default()))),
                "lowercase" => Ok(Val::String(s.to_lowercase())),
                "uppercase" => Ok(Val::String(s.to_uppercase())),
                "count" => Ok(Val::Int(s.chars().count() as i64)),
                "empty" => Ok(Val::Bool(s.is_empty())),
                _ => Ok(Val::Nil),
            },
            Val::List(items) => match method {
                "count" => Ok(Val::Int(items.len() as i64)),
                "empty" => Ok(Val::Bool(items.is_empty())),
                "map" => {
                    if let Some(Val::Closure(params, body, cenv)) = args.first() {
                        let mut results = Vec::new();
                        for item in items {
                            results.push(self.invoke_closure(params, body, cenv, &[item.clone()])?);
                        }
                        Ok(Val::List(results))
                    } else { Ok(Val::Nil) }
                }
                "filter" => {
                    if let Some(Val::Closure(params, body, cenv)) = args.first() {
                        let mut results = Vec::new();
                        for item in items {
                            if self.invoke_closure(params, body, cenv, &[item.clone()])?.is_truthy() {
                                results.push(item.clone());
                            }
                        }
                        Ok(Val::List(results))
                    } else { Ok(Val::Nil) }
                }
                _ => Ok(Val::Nil),
            },
            Val::Dict(map) => match method {
                "keys" => Ok(Val::List(map.keys().map(|k| Val::String(k.clone())).collect())),
                "values" => Ok(Val::List(map.values().cloned().collect())),
                "has" => Ok(Val::Bool(map.contains_key(&args.first().map(|a| a.as_string()).unwrap_or_default()))),
                "count" => Ok(Val::Int(map.len() as i64)),
                _ => Ok(Val::Nil),
            },
            _ => Ok(Val::Nil),
        }
    }

    fn eval_binary(&mut self, op: &str, left: &Node, right: &Node) -> Result<Val, EvalStop> {
        if op == "??" {
            let lv = self.eval(left)?;
            return if matches!(lv, Val::Nil) { self.eval(right) } else { Ok(lv) };
        }
        if op == "&&" {
            return Ok(Val::Bool(self.eval(left)?.is_truthy() && self.eval(right)?.is_truthy()));
        }
        if op == "||" {
            let lv = self.eval(left)?;
            return if lv.is_truthy() { Ok(Val::Bool(true)) } else { Ok(Val::Bool(self.eval(right)?.is_truthy())) };
        }
        if op == "|>" {
            let lv = self.eval(left)?;
            let rv = self.eval(right)?;
            if let Val::Closure(params, body, cenv) = rv {
                return self.invoke_closure(&params, &body, &cenv, &[lv]);
            }
            if let Node::Identifier(name) = right {
                if let Some(Val::Closure(params, body, cenv)) = self.env.get(name) {
                    return self.invoke_closure(&params, &body, &cenv, &[lv]);
                }
            }
            return Ok(Val::Nil);
        }

        let lv = self.eval(left)?;
        let rv = self.eval(right)?;

        match op {
            "==" => Ok(Val::Bool(vals_equal(&lv, &rv))),
            "!=" => Ok(Val::Bool(!vals_equal(&lv, &rv))),
            "+" => match (&lv, &rv) {
                (Val::Int(a), Val::Int(b)) => Ok(Val::Int(a + b)),
                (Val::Float(a), Val::Float(b)) => Ok(Val::Float(a + b)),
                (Val::Int(a), Val::Float(b)) => Ok(Val::Float(*a as f64 + b)),
                (Val::Float(a), Val::Int(b)) => Ok(Val::Float(a + *b as f64)),
                (Val::String(a), _) => Ok(Val::String(format!("{a}{}", rv.as_string()))),
                _ => Ok(Val::String(format!("{}{}", lv.as_string(), rv.as_string()))),
            },
            "-" => match (&lv, &rv) {
                (Val::Int(a), Val::Int(b)) => Ok(Val::Int(a - b)),
                (Val::Float(a), Val::Float(b)) => Ok(Val::Float(a - b)),
                (Val::Int(a), Val::Float(b)) => Ok(Val::Float(*a as f64 - b)),
                (Val::Float(a), Val::Int(b)) => Ok(Val::Float(a - *b as f64)),
                _ => Ok(Val::Nil),
            },
            "*" => match (&lv, &rv) {
                (Val::Int(a), Val::Int(b)) => Ok(Val::Int(a * b)),
                (Val::Float(a), Val::Float(b)) => Ok(Val::Float(a * b)),
                (Val::Int(a), Val::Float(b)) => Ok(Val::Float(*a as f64 * b)),
                (Val::Float(a), Val::Int(b)) => Ok(Val::Float(a * *b as f64)),
                _ => Ok(Val::Nil),
            },
            "/" => match (&lv, &rv) {
                (Val::Int(a), Val::Int(b)) if *b != 0 => Ok(Val::Int(a / b)),
                (Val::Float(a), Val::Float(b)) if *b != 0.0 => Ok(Val::Float(a / b)),
                _ => Ok(Val::Nil),
            },
            "<" | ">" | "<=" | ">=" => {
                let cmp = match (&lv, &rv) {
                    (Val::Int(a), Val::Int(b)) => a.cmp(b) as i32,
                    (Val::Float(a), Val::Float(b)) => if a < b { -1 } else if a > b { 1 } else { 0 },
                    (Val::Int(a), Val::Float(b)) => { let a = *a as f64; if a < *b { -1 } else if a > *b { 1 } else { 0 } },
                    (Val::Float(a), Val::Int(b)) => { let b = *b as f64; if *a < b { -1 } else if *a > b { 1 } else { 0 } },
                    _ => 0,
                };
                Ok(Val::Bool(match op {
                    "<" => cmp < 0,
                    ">" => cmp > 0,
                    "<=" => cmp <= 0,
                    ">=" => cmp >= 0,
                    _ => false,
                }))
            },
            _ => Ok(Val::Nil),
        }
    }
}

enum EvalStop {
    Return(Val),
    Error(String),
}
