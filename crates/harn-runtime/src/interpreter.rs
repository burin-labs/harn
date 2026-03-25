use std::collections::BTreeMap;

use harn_lexer::{Lexer, StringSegment};
use harn_parser::{MatchArm, Node, Parser};

use crate::environment::Environment;
use crate::error::RuntimeError;
use crate::value::{compare_values, values_equal, Value};

/// Builtin function signature. Takes args and output buffer, returns a value.
pub type BuiltinFn = Box<dyn Fn(&[Value], &mut Vec<u8>) -> Result<Value, RuntimeError>>;

/// The Harn tree-walking interpreter.
pub struct Interpreter {
    env: Environment,
    pipelines: BTreeMap<String, Node>,
    builtins: BTreeMap<String, BuiltinFn>,
    output: Vec<u8>,
}

impl Default for Interpreter {
    fn default() -> Self {
        Self::new()
    }
}

impl Interpreter {
    pub fn new() -> Self {
        Self {
            env: Environment::new(),
            pipelines: BTreeMap::new(),
            builtins: BTreeMap::new(),
            output: Vec::new(),
        }
    }

    /// Register a builtin function.
    pub fn register_builtin<F>(&mut self, name: &str, f: F)
    where
        F: Fn(&[Value], &mut Vec<u8>) -> Result<Value, RuntimeError> + 'static,
    {
        self.builtins.insert(name.to_string(), Box::new(f));
    }

    /// Get captured output.
    pub fn output(&self) -> &[u8] {
        &self.output
    }

    /// Take and return captured output, clearing the buffer.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output)
    }

    /// Run a parsed program.
    pub fn run(&mut self, program: &[Node]) -> Result<(), RuntimeError> {
        // Register all pipelines
        for node in program {
            if let Node::Pipeline { name, .. } = node {
                self.pipelines.insert(name.clone(), node.clone());
            }
        }

        // Find entry pipeline: "default" or first pipeline
        let main = self.pipelines.get("default").cloned().or_else(|| {
            program
                .iter()
                .find(|n| matches!(n, Node::Pipeline { .. }))
                .cloned()
        });

        let Some(main) = main else { return Ok(()) };

        if let Node::Pipeline {
            params,
            body,
            extends,
            ..
        } = &main
        {
            let pipeline_env = self.env.child();

            // Bind pipeline parameters
            if params.iter().any(|p| p == "task") {
                pipeline_env.define("task", Value::String(String::new()), false);
            }
            if params.iter().any(|p| p == "project") {
                pipeline_env.define("project", Value::String(String::new()), false);
            }

            // Inject context dict
            let ctx = BTreeMap::from([
                ("task".to_string(), Value::String(String::new())),
                ("project_root".to_string(), Value::String(String::new())),
                ("task_type".to_string(), Value::String(String::new())),
            ]);
            pipeline_env.define("context", Value::Dict(ctx), false);

            // Handle extends
            let resolved_body = if let Some(parent_name) = extends {
                if let Some(parent) = self.pipelines.get(parent_name).cloned() {
                    self.resolve_inheritance(body, &parent)
                } else {
                    body.clone()
                }
            } else {
                body.clone()
            };

            let result = self.with_env(pipeline_env, |interp| {
                interp.exec_statements(&resolved_body)
            });

            match result {
                Ok(_) | Err(RuntimeError::ReturnValue(_)) => Ok(()),
                Err(e) => Err(e),
            }
        } else {
            Ok(())
        }
    }

    fn exec_statements(&mut self, stmts: &[Node]) -> Result<Value, RuntimeError> {
        let mut result = Value::Nil;
        for stmt in stmts {
            result = self.eval(stmt)?;
        }
        Ok(result)
    }

    /// Execute a closure with a temporary environment, restoring the original afterward.
    fn with_env<F, R>(&mut self, new_env: Environment, f: F) -> R
    where
        F: FnOnce(&mut Self) -> R,
    {
        let saved = self.env.clone();
        self.env = new_env;
        let result = f(self);
        self.env = saved;
        result
    }

    fn eval(&mut self, node: &Node) -> Result<Value, RuntimeError> {
        match node {
            Node::LetBinding { name, value } => {
                let val = self.eval(value)?;
                self.env.define(name, val, false);
                Ok(Value::Nil)
            }

            Node::VarBinding { name, value } => {
                let val = self.eval(value)?;
                self.env.define(name, val, true);
                Ok(Value::Nil)
            }

            Node::Assignment { target, value } => {
                let val = self.eval(value)?;
                if let Node::Identifier(name) = target.as_ref() {
                    self.env.assign(name, val)?;
                }
                Ok(Value::Nil)
            }

            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                let cond = self.eval(condition)?;
                if cond.is_truthy() {
                    self.exec_statements(then_body)
                } else if let Some(else_body) = else_body {
                    self.exec_statements(else_body)
                } else {
                    Ok(Value::Nil)
                }
            }

            Node::ForIn {
                variable,
                iterable,
                body,
            } => self.eval_for_in(variable, iterable, body),

            Node::MatchExpr { value, arms } => self.eval_match(value, arms),

            Node::WhileLoop { condition, body } => self.eval_while(condition, body),

            Node::Retry { count, body } => self.eval_retry(count, body),

            Node::Parallel {
                count,
                variable,
                body,
            } => {
                // Sequential fallback (no async runtime)
                let count_val = self.eval(count)?;
                let n = count_val.as_int().unwrap_or(1);
                let mut results = Vec::new();
                for i in 0..n {
                    let task_env = self.env.child();
                    if let Some(var) = variable {
                        task_env.define(var, Value::Int(i), false);
                    }
                    results.push(self.with_env(task_env, |interp| interp.exec_statements(body))?);
                }
                Ok(Value::List(results))
            }

            Node::ParallelMap {
                list,
                variable,
                body,
            } => {
                let list_val = self.eval(list)?;
                let items = match list_val {
                    Value::List(items) => items,
                    _ => return Ok(Value::Nil),
                };
                let mut results = Vec::new();
                for item in items {
                    let task_env = self.env.child();
                    task_env.define(variable, item, false);
                    results.push(self.with_env(task_env, |interp| interp.exec_statements(body))?);
                }
                Ok(Value::List(results))
            }

            Node::ReturnStmt { value } => {
                let val = value.as_ref().map(|v| self.eval(v)).transpose()?;
                Err(RuntimeError::ReturnValue(val))
            }

            Node::FunctionCall { name, args } => self.eval_function_call(name, args),

            Node::MethodCall {
                object,
                method,
                args,
            } => self.eval_method_call(object, method, args),

            Node::PropertyAccess { object, property } => {
                self.eval_property_access(object, property)
            }

            Node::SubscriptAccess { object, index } => {
                let obj = self.eval(object)?;
                let idx = self.eval(index)?;
                match (&obj, &idx) {
                    (Value::List(items), Value::Int(i)) => {
                        if *i < 0 {
                            return Ok(Value::Nil);
                        }
                        Ok(items.get(*i as usize).cloned().unwrap_or(Value::Nil))
                    }
                    (Value::Dict(map), _) => {
                        Ok(map.get(&idx.as_string()).cloned().unwrap_or(Value::Nil))
                    }
                    _ => Ok(Value::Nil),
                }
            }

            Node::BinaryOp { op, left, right } => self.eval_binary_op(op, left, right),

            Node::Ternary {
                condition,
                true_expr,
                false_expr,
            } => {
                let cond = self.eval(condition)?;
                if cond.is_truthy() {
                    self.eval(true_expr)
                } else {
                    self.eval(false_expr)
                }
            }

            Node::UnaryOp { op, operand } => {
                let val = self.eval(operand)?;
                match op.as_str() {
                    "!" => Ok(Value::Bool(!val.is_truthy())),
                    "-" => match val {
                        Value::Int(n) => Ok(Value::Int(-n)),
                        Value::Float(n) => Ok(Value::Float(-n)),
                        _ => Ok(Value::Nil),
                    },
                    _ => Ok(Value::Nil),
                }
            }

            Node::ThrowStmt { value } => {
                let val = self.eval(value)?;
                Err(RuntimeError::ThrownError(val))
            }

            Node::InterpolatedString(segments) => self.eval_interpolated_string(segments),

            Node::StringLiteral(s) => Ok(Value::String(s.clone())),
            Node::IntLiteral(n) => Ok(Value::Int(*n)),
            Node::FloatLiteral(n) => Ok(Value::Float(*n)),
            Node::BoolLiteral(b) => Ok(Value::Bool(*b)),
            Node::NilLiteral => Ok(Value::Nil),

            Node::Identifier(name) => Ok(self.env.get(name).unwrap_or(Value::Nil)),

            Node::ListLiteral(elements) => {
                let values: Result<Vec<_>, _> = elements.iter().map(|el| self.eval(el)).collect();
                Ok(Value::List(values?))
            }

            Node::DictLiteral(entries) => {
                let mut map = BTreeMap::new();
                for entry in entries {
                    let key = self.eval(&entry.key)?;
                    let val = self.eval(&entry.value)?;
                    map.insert(key.as_string(), val);
                }
                Ok(Value::Dict(map))
            }

            Node::Block(stmts) => {
                let block_env = self.env.child();
                self.with_env(block_env, |interp| interp.exec_statements(stmts))
            }

            Node::Closure { params, body } => Ok(Value::Closure {
                params: params.clone(),
                body: body.clone(),
                env: self.env.clone(),
            }),

            Node::TryCatch {
                body,
                error_var,
                catch_body,
            } => self.eval_try_catch(body, error_var, catch_body),

            Node::FnDecl { name, params, body } => {
                let fn_value = Value::Closure {
                    params: params.clone(),
                    body: body.clone(),
                    env: self.env.clone(),
                };
                self.env.define(name, fn_value, false);
                Ok(Value::Nil)
            }

            Node::SpawnExpr { .. } => {
                // No async runtime — return nil for now
                Ok(Value::Nil)
            }

            Node::ImportDecl { .. } | Node::Pipeline { .. } | Node::OverrideDecl { .. } => {
                Ok(Value::Nil)
            }
        }
    }

    // --- Control flow ---

    fn eval_for_in(
        &mut self,
        variable: &str,
        iterable: &Node,
        body: &[Node],
    ) -> Result<Value, RuntimeError> {
        let iter_val = self.eval(iterable)?;

        let items: Vec<Value> = match iter_val {
            Value::List(items) => items,
            Value::Dict(map) => map
                .into_iter()
                .map(|(k, v)| {
                    Value::Dict(BTreeMap::from([
                        ("key".to_string(), Value::String(k)),
                        ("value".to_string(), v),
                    ]))
                })
                .collect(),
            _ => return Ok(Value::Nil),
        };

        let loop_env = self.env.child();
        let saved = self.env.clone();
        self.env = loop_env;

        let mut result = Value::Nil;
        for item in items {
            self.env.define(variable, item, true);
            result = self.exec_statements(body)?;
        }

        self.env = saved;
        Ok(result)
    }

    fn eval_match(&mut self, value: &Node, arms: &[MatchArm]) -> Result<Value, RuntimeError> {
        let val = self.eval(value)?;
        for arm in arms {
            let pattern_val = self.eval(&arm.pattern)?;
            if values_equal(&val, &pattern_val) {
                return self.exec_statements(&arm.body);
            }
        }
        Ok(Value::Nil)
    }

    fn eval_while(&mut self, condition: &Node, body: &[Node]) -> Result<Value, RuntimeError> {
        let mut result = Value::Nil;
        let max_iterations = 10_000;
        let mut iteration = 0;
        while iteration < max_iterations {
            let cond = self.eval(condition)?;
            if !cond.is_truthy() {
                break;
            }
            let loop_env = self.env.child();
            result = self.with_env(loop_env, |interp| interp.exec_statements(body))?;
            iteration += 1;
        }
        Ok(result)
    }

    fn eval_retry(&mut self, count_node: &Node, body: &[Node]) -> Result<Value, RuntimeError> {
        let count_val = self.eval(count_node)?;
        let count = count_val.as_int().unwrap_or(3) as usize;

        for _attempt in 0..count {
            match self.exec_statements(body) {
                Ok(result) => return Ok(result),
                Err(RuntimeError::ReturnValue(val)) => {
                    return Err(RuntimeError::ReturnValue(val));
                }
                Err(_) => {
                    // Retry on error (spec: does not re-throw, returns nil after exhaustion)
                }
            }
        }
        Ok(Value::Nil)
    }

    fn eval_try_catch(
        &mut self,
        body: &[Node],
        error_var: &Option<String>,
        catch_body: &[Node],
    ) -> Result<Value, RuntimeError> {
        match self.exec_statements(body) {
            Ok(val) => Ok(val),
            Err(RuntimeError::ReturnValue(val)) => Err(RuntimeError::ReturnValue(val)),
            Err(err) => {
                let error_value = match err {
                    RuntimeError::ThrownError(v) => v,
                    other => Value::String(other.to_string()),
                };
                let catch_env = self.env.child();
                if let Some(var) = error_var {
                    catch_env.define(var, error_value, false);
                }
                self.with_env(catch_env, |interp| interp.exec_statements(catch_body))
            }
        }
    }

    // --- Function calls ---

    fn eval_function_call(&mut self, name: &str, args: &[Node]) -> Result<Value, RuntimeError> {
        // Check for user-defined function (closure) first
        if let Some(Value::Closure { params, body, env }) = self.env.get(name) {
            let arg_values = self.eval_args(args)?;
            return self.invoke_closure(&params, &body, &env, &arg_values);
        }

        // Check builtins — test membership first, eval args, then call
        if self.builtins.contains_key(name) {
            let arg_values = self.eval_args(args)?;
            let builtin = &self.builtins[name];
            return builtin(&arg_values, &mut self.output);
        }

        Err(RuntimeError::UndefinedBuiltin(name.to_string()))
    }

    /// Evaluate a list of argument nodes into values.
    fn eval_args(&mut self, args: &[Node]) -> Result<Vec<Value>, RuntimeError> {
        args.iter().map(|arg| self.eval(arg)).collect()
    }

    fn invoke_closure(
        &mut self,
        params: &[String],
        body: &[Node],
        captured_env: &Environment,
        args: &[Value],
    ) -> Result<Value, RuntimeError> {
        let call_env = captured_env.child();
        for (i, param) in params.iter().enumerate() {
            let val = args.get(i).cloned().unwrap_or(Value::Nil);
            call_env.define(param, val, false);
        }
        let result = self.with_env(call_env, |interp| interp.exec_statements(body));
        match result {
            Ok(val) => Ok(val),
            Err(RuntimeError::ReturnValue(val)) => Ok(val.unwrap_or(Value::Nil)),
            Err(e) => Err(e),
        }
    }

    // --- Method calls ---

    fn eval_method_call(
        &mut self,
        object: &Node,
        method: &str,
        args: &[Node],
    ) -> Result<Value, RuntimeError> {
        let obj = self.eval(object)?;
        let arg_values = self.eval_args(args)?;

        // Check for method-style builtins: obj.method(args) → builtin "obj.method"
        if let Node::Identifier(obj_name) = object {
            let qualified = format!("{obj_name}.{method}");
            if self.builtins.contains_key(&qualified) {
                let builtin = &self.builtins[&qualified];
                return builtin(&arg_values, &mut self.output);
            }
        }

        match obj {
            Value::String(s) => self.eval_string_method(&s, method, &arg_values),
            Value::List(items) => self.eval_list_method(&items, method, &arg_values),
            Value::Dict(map) => self.eval_dict_method(&map, method, &arg_values),
            _ => Ok(Value::Nil),
        }
    }

    // --- String methods ---

    fn eval_string_method(
        &mut self,
        s: &str,
        method: &str,
        args: &[Value],
    ) -> Result<Value, RuntimeError> {
        match method {
            "count" => Ok(Value::Int(s.chars().count() as i64)),
            "empty" => Ok(Value::Bool(s.is_empty())),
            "contains" => Ok(Value::Bool(s.contains(&arg_string(args, 0)))),
            "replace" => {
                if args.len() >= 2 {
                    Ok(Value::String(
                        s.replace(&args[0].as_string(), &args[1].as_string()),
                    ))
                } else {
                    Ok(Value::String(s.to_string()))
                }
            }
            "split" => {
                let sep = if args.is_empty() {
                    ",".to_string()
                } else {
                    args[0].as_string()
                };
                let parts: Vec<Value> = s
                    .split(&sep)
                    .map(|p| Value::String(p.to_string()))
                    .collect();
                Ok(Value::List(parts))
            }
            "trim" => Ok(Value::String(s.trim().to_string())),
            "starts_with" => Ok(Value::Bool(s.starts_with(&arg_string(args, 0)))),
            "ends_with" => Ok(Value::Bool(s.ends_with(&arg_string(args, 0)))),
            "lowercase" => Ok(Value::String(s.to_lowercase())),
            "uppercase" => Ok(Value::String(s.to_uppercase())),
            "substring" => {
                let start_val = args.first().and_then(|a| a.as_int()).unwrap_or(0);
                let char_count = s.chars().count() as i64;
                let start = start_val.max(0).min(char_count) as usize;
                let end = if args.len() > 1 {
                    args[1].as_int().unwrap_or(char_count).min(char_count) as usize
                } else {
                    char_count as usize
                };
                let end = end.max(start);
                let result: String = s.chars().skip(start).take(end - start).collect();
                Ok(Value::String(result))
            }
            _ => Ok(Value::Nil),
        }
    }

    // --- List methods ---

    fn eval_list_method(
        &mut self,
        items: &[Value],
        method: &str,
        args: &[Value],
    ) -> Result<Value, RuntimeError> {
        match method {
            "count" => Ok(Value::Int(items.len() as i64)),
            "empty" => Ok(Value::Bool(items.is_empty())),
            "map" => self.list_apply(items, args, |results, item, _truthy| {
                results.push(item);
            }),
            "filter" => {
                let closure = require_closure(args)?;
                let mut results = Vec::new();
                for item in items {
                    let result = self.invoke_closure_item(closure, item)?;
                    if result.is_truthy() {
                        results.push(item.clone());
                    }
                }
                Ok(Value::List(results))
            }
            "reduce" => {
                if args.len() >= 2 {
                    if let Value::Closure { params, body, env } = &args[1] {
                        let mut acc = args[0].clone();
                        for item in items {
                            acc = self.invoke_closure(params, body, env, &[acc, item.clone()])?;
                        }
                        return Ok(acc);
                    }
                }
                Ok(Value::Nil)
            }
            "find" => {
                let closure = require_closure(args)?;
                for item in items {
                    let result = self.invoke_closure_item(closure, item)?;
                    if result.is_truthy() {
                        return Ok(item.clone());
                    }
                }
                Ok(Value::Nil)
            }
            "any" => {
                let closure = require_closure(args)?;
                for item in items {
                    let result = self.invoke_closure_item(closure, item)?;
                    if result.is_truthy() {
                        return Ok(Value::Bool(true));
                    }
                }
                Ok(Value::Bool(false))
            }
            "all" => {
                let closure = require_closure(args)?;
                for item in items {
                    let result = self.invoke_closure_item(closure, item)?;
                    if !result.is_truthy() {
                        return Ok(Value::Bool(false));
                    }
                }
                Ok(Value::Bool(true))
            }
            "flat_map" => {
                let closure = require_closure(args)?;
                let mut results = Vec::new();
                for item in items {
                    let result = self.invoke_closure_item(closure, item)?;
                    if let Value::List(inner) = result {
                        results.extend(inner);
                    } else {
                        results.push(result);
                    }
                }
                Ok(Value::List(results))
            }
            _ => Ok(Value::Nil),
        }
    }

    /// Helper: invoke a closure with a single item argument.
    #[allow(clippy::cloned_ref_to_slice_refs)]
    fn invoke_closure_item(
        &mut self,
        closure: (&[String], &[Node], &Environment),
        item: &Value,
    ) -> Result<Value, RuntimeError> {
        let (params, body, env) = closure;
        self.invoke_closure(params, body, env, &[item.clone()])
    }

    /// Helper: map over items with a closure, collecting results.
    fn list_apply<F>(
        &mut self,
        items: &[Value],
        args: &[Value],
        mut collect: F,
    ) -> Result<Value, RuntimeError>
    where
        F: FnMut(&mut Vec<Value>, Value, bool),
    {
        let closure = require_closure(args)?;
        let mut results = Vec::new();
        for item in items {
            let result = self.invoke_closure_item(closure, item)?;
            let truthy = result.is_truthy();
            collect(&mut results, result, truthy);
        }
        Ok(Value::List(results))
    }

    // --- Dict methods ---

    fn eval_dict_method(
        &mut self,
        map: &BTreeMap<String, Value>,
        method: &str,
        args: &[Value],
    ) -> Result<Value, RuntimeError> {
        match method {
            "keys" => Ok(Value::List(
                map.keys().map(|k| Value::String(k.clone())).collect(),
            )),
            "values" => Ok(Value::List(map.values().cloned().collect())),
            "entries" => Ok(Value::List(
                map.iter()
                    .map(|(k, v)| {
                        Value::Dict(BTreeMap::from([
                            ("key".to_string(), Value::String(k.clone())),
                            ("value".to_string(), v.clone()),
                        ]))
                    })
                    .collect(),
            )),
            "count" => Ok(Value::Int(map.len() as i64)),
            "has" => Ok(Value::Bool(map.contains_key(&arg_string(args, 0)))),
            "merge" => {
                if let Some(Value::Dict(other)) = args.first() {
                    let mut result = map.clone();
                    result.extend(other.iter().map(|(k, v)| (k.clone(), v.clone())));
                    Ok(Value::Dict(result))
                } else {
                    Ok(Value::Dict(map.clone()))
                }
            }
            "map_values" => {
                let closure = require_closure(args)?;
                let mut result = BTreeMap::new();
                for (k, v) in map {
                    result.insert(k.clone(), self.invoke_closure_item(closure, v)?);
                }
                Ok(Value::Dict(result))
            }
            "filter" => {
                let closure = require_closure(args)?;
                let mut result = BTreeMap::new();
                for (k, v) in map {
                    let keep = self.invoke_closure_item(closure, v)?;
                    if keep.is_truthy() {
                        result.insert(k.clone(), v.clone());
                    }
                }
                Ok(Value::Dict(result))
            }
            _ => Ok(Value::Nil),
        }
    }

    // --- Property access ---

    fn eval_property_access(
        &mut self,
        object: &Node,
        property: &str,
    ) -> Result<Value, RuntimeError> {
        let obj = self.eval(object)?;
        match &obj {
            Value::Dict(map) => Ok(map.get(property).cloned().unwrap_or(Value::Nil)),
            Value::List(items) => match property {
                "count" => Ok(Value::Int(items.len() as i64)),
                "empty" => Ok(Value::Bool(items.is_empty())),
                "first" => Ok(items.first().cloned().unwrap_or(Value::Nil)),
                "last" => Ok(items.last().cloned().unwrap_or(Value::Nil)),
                _ => Ok(Value::Nil),
            },
            Value::String(s) => match property {
                "count" => Ok(Value::Int(s.chars().count() as i64)),
                "empty" => Ok(Value::Bool(s.is_empty())),
                _ => Ok(Value::Nil),
            },
            _ => Ok(Value::Nil),
        }
    }

    // --- Binary ops ---

    fn eval_binary_op(
        &mut self,
        op: &str,
        left: &Node,
        right: &Node,
    ) -> Result<Value, RuntimeError> {
        // Pipe operator
        if op == "|>" {
            let left_val = self.eval(left)?;
            let right_val = self.eval(right)?;
            // If right is a closure, invoke it
            if let Value::Closure { params, body, env } = &right_val {
                return self.invoke_closure(params, body, env, &[left_val]);
            }
            // If right is an identifier, check for builtin or closure variable
            if let Node::Identifier(name) = right {
                if self.builtins.contains_key(name.as_str()) {
                    let builtin = &self.builtins[name.as_str()];
                    return builtin(&[left_val], &mut self.output);
                }
                if let Some(Value::Closure { params, body, env }) = self.env.get(name) {
                    return self.invoke_closure(&params, &body, &env, &[left_val]);
                }
            }
            return Ok(Value::Nil);
        }

        // Nil coalescing (short-circuit)
        if op == "??" {
            let left_val = self.eval(left)?;
            if matches!(left_val, Value::Nil) {
                return self.eval(right);
            }
            return Ok(left_val);
        }

        // Logical AND (short-circuit)
        if op == "&&" {
            let left_val = self.eval(left)?;
            if !left_val.is_truthy() {
                return Ok(Value::Bool(false));
            }
            return Ok(Value::Bool(self.eval(right)?.is_truthy()));
        }

        // Logical OR (short-circuit)
        if op == "||" {
            let left_val = self.eval(left)?;
            if left_val.is_truthy() {
                return Ok(Value::Bool(true));
            }
            return Ok(Value::Bool(self.eval(right)?.is_truthy()));
        }

        let left_val = self.eval(left)?;
        let right_val = self.eval(right)?;

        match op {
            "==" => Ok(Value::Bool(values_equal(&left_val, &right_val))),
            "!=" => Ok(Value::Bool(!values_equal(&left_val, &right_val))),
            "<" => Ok(Value::Bool(compare_values(&left_val, &right_val) < 0)),
            ">" => Ok(Value::Bool(compare_values(&left_val, &right_val) > 0)),
            "<=" => Ok(Value::Bool(compare_values(&left_val, &right_val) <= 0)),
            ">=" => Ok(Value::Bool(compare_values(&left_val, &right_val) >= 0)),
            "+" => match (&left_val, &right_val) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_add(*b))),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
                (Value::String(a), _) => Ok(Value::String(format!("{a}{}", right_val.as_string()))),
                (Value::List(a), Value::List(b)) => {
                    let mut result = a.clone();
                    result.extend(b.iter().cloned());
                    Ok(Value::List(result))
                }
                _ => Ok(Value::String(format!(
                    "{}{}",
                    left_val.as_string(),
                    right_val.as_string()
                ))),
            },
            "-" => match (&left_val, &right_val) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_sub(*b))),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),
                _ => Ok(Value::Nil),
            },
            "*" => match (&left_val, &right_val) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_mul(*b))),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
                _ => Ok(Value::Nil),
            },
            "/" => match (&left_val, &right_val) {
                (Value::Int(a), Value::Int(b)) if *b != 0 => Ok(Value::Int(a / b)),
                (Value::Float(a), Value::Float(b)) if *b != 0.0 => Ok(Value::Float(a / b)),
                _ => Ok(Value::Nil), // spec: division by zero returns nil
            },
            _ => Ok(Value::Nil),
        }
    }

    // --- Interpolated strings ---

    fn eval_interpolated_string(
        &mut self,
        segments: &[StringSegment],
    ) -> Result<Value, RuntimeError> {
        let mut result = String::new();
        for segment in segments {
            match segment {
                StringSegment::Literal(s) => result.push_str(s),
                StringSegment::Expression(expr_str) => {
                    let mut lexer = Lexer::new(expr_str);
                    let tokens = lexer
                        .tokenize()
                        .map_err(|e| RuntimeError::ThrownError(Value::String(e.to_string())))?;
                    let mut parser = Parser::new(tokens);
                    let node = parser
                        .parse_single_expression()
                        .map_err(|e| RuntimeError::ThrownError(Value::String(e.to_string())))?;
                    let value = self.eval(&node)?;
                    result.push_str(&value.as_string());
                }
            }
        }
        Ok(Value::String(result))
    }

    // --- Pipeline inheritance ---

    fn resolve_inheritance(&self, child: &[Node], parent: &Node) -> Vec<Node> {
        let parent_body = if let Node::Pipeline { body, .. } = parent {
            body
        } else {
            return child.to_vec();
        };

        let has_overrides = child.iter().any(|n| matches!(n, Node::OverrideDecl { .. }));

        if !has_overrides {
            return child.to_vec();
        }

        let non_overrides: Vec<Node> = child
            .iter()
            .filter(|n| !matches!(n, Node::OverrideDecl { .. }))
            .cloned()
            .collect();

        let mut result = parent_body.clone();
        result.extend(non_overrides);
        result
    }
}

/// Extract a string argument at the given index, defaulting to empty string.
fn arg_string(args: &[Value], index: usize) -> String {
    args.get(index).map(|a| a.as_string()).unwrap_or_default()
}

/// Extract closure components from the first argument, returning Nil if not a closure.
fn require_closure(args: &[Value]) -> Result<(&[String], &[Node], &Environment), RuntimeError> {
    match args.first() {
        Some(Value::Closure { params, body, env }) => Ok((params, body, env)),
        _ => Err(RuntimeError::TypeMismatch {
            expected: "closure".to_string(),
            got: args.first().cloned().unwrap_or(Value::Nil),
        }),
    }
}
