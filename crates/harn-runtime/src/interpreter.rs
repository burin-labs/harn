use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use harn_lexer::{Lexer, StringSegment};
use harn_parser::{MatchArm, Node, Parser};

use crate::environment::Environment;
use crate::error::RuntimeError;
use crate::value::{compare_values, values_equal, Value};

/// Sync builtin function signature.
pub type BuiltinFn =
    Arc<dyn Fn(&[Value], &mut Vec<u8>) -> Result<Value, RuntimeError> + Send + Sync>;

/// Async builtin function signature.
pub type AsyncBuiltinFn = Arc<
    dyn Fn(Vec<Value>) -> Pin<Box<dyn Future<Output = Result<Value, RuntimeError>>>> + Send + Sync,
>;

/// Result of a spawned task: value + captured output.
type TaskResult = Result<(Value, Vec<u8>), RuntimeError>;

/// Shared state for spawned tasks.
type SpawnedTasks = Arc<Mutex<BTreeMap<String, tokio::task::JoinHandle<TaskResult>>>>;

static TASK_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The Harn tree-walking async interpreter.
pub struct Interpreter {
    env: Environment,
    pipelines: BTreeMap<String, Node>,
    builtins: Arc<BTreeMap<String, BuiltinFn>>,
    async_builtins: Arc<BTreeMap<String, AsyncBuiltinFn>>,
    output: Vec<u8>,
    /// Base directory for resolving relative imports.
    source_dir: PathBuf,
    /// Track imported files to prevent cycles.
    imported: Vec<PathBuf>,
    /// Spawned task handles.
    spawned: SpawnedTasks,
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
            builtins: Arc::new(BTreeMap::new()),
            async_builtins: Arc::new(BTreeMap::new()),
            output: Vec::new(),
            source_dir: PathBuf::from("."),
            imported: Vec::new(),
            spawned: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Create a child interpreter for parallel/spawn tasks.
    /// Shares builtins and spawned-task registry, gets an isolated env and output.
    fn child_interpreter(&self, child_env: Environment) -> Self {
        Self {
            env: child_env,
            pipelines: self.pipelines.clone(),
            builtins: Arc::clone(&self.builtins),
            async_builtins: Arc::clone(&self.async_builtins),
            output: Vec::new(),
            source_dir: self.source_dir.clone(),
            imported: self.imported.clone(),
            spawned: Arc::clone(&self.spawned),
        }
    }

    /// Register a sync builtin function.
    pub fn register_builtin<F>(&mut self, name: &str, f: F)
    where
        F: Fn(&[Value], &mut Vec<u8>) -> Result<Value, RuntimeError> + Send + Sync + 'static,
    {
        Arc::get_mut(&mut self.builtins)
            .expect("cannot register builtins after spawning tasks")
            .insert(name.to_string(), Arc::new(f));
    }

    /// Register an async builtin function.
    pub fn register_async_builtin<F, Fut>(&mut self, name: &str, f: F)
    where
        F: Fn(Vec<Value>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, RuntimeError>> + 'static,
    {
        Arc::get_mut(&mut self.async_builtins)
            .expect("cannot register builtins after spawning tasks")
            .insert(
                name.to_string(),
                Arc::new(move |args: Vec<Value>| -> Pin<Box<dyn Future<Output = Result<Value, RuntimeError>>>> {
                    Box::pin(f(args))
                }),
            );
    }

    /// Check if a builtin exists (sync or async).
    #[allow(dead_code)]
    pub fn has_builtin(&self, name: &str) -> bool {
        self.builtins.contains_key(name) || self.async_builtins.contains_key(name)
    }

    /// Set the base directory for resolving relative imports.
    pub fn set_source_dir(&mut self, dir: impl Into<PathBuf>) {
        self.source_dir = dir.into();
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
    pub async fn run(&mut self, program: &[Node]) -> Result<(), RuntimeError> {
        // Register all pipelines and process imports
        for node in program {
            if let Node::Pipeline { name, .. } = node {
                self.pipelines.insert(name.clone(), node.clone());
            } else if let Node::ImportDecl { path } = node {
                self.eval_import(path).await?;
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

            if params.iter().any(|p| p == "task") {
                pipeline_env.define("task", Value::String(String::new()), false);
            }
            if params.iter().any(|p| p == "project") {
                pipeline_env.define("project", Value::String(String::new()), false);
            }

            let ctx = BTreeMap::from([
                ("task".to_string(), Value::String(String::new())),
                ("project_root".to_string(), Value::String(String::new())),
                ("task_type".to_string(), Value::String(String::new())),
            ]);
            pipeline_env.define("context", Value::Dict(ctx), false);

            let resolved_body = if let Some(parent_name) = extends {
                if let Some(parent) = self.pipelines.get(parent_name).cloned() {
                    self.resolve_inheritance(body, &parent)
                } else {
                    body.clone()
                }
            } else {
                body.clone()
            };

            let result = self.exec_in_env(pipeline_env, &resolved_body).await;

            match result {
                Ok(_) | Err(RuntimeError::ReturnValue(_)) => Ok(()),
                Err(e) => Err(e),
            }
        } else {
            Ok(())
        }
    }

    async fn exec_statements(&mut self, stmts: &[Node]) -> Result<Value, RuntimeError> {
        let mut result = Value::Nil;
        for stmt in stmts {
            result = self.eval(stmt).await?;
        }
        Ok(result)
    }

    /// Execute statements in a child environment, restoring afterward.
    async fn exec_in_env(
        &mut self,
        env: Environment,
        stmts: &[Node],
    ) -> Result<Value, RuntimeError> {
        let saved = self.env.clone();
        self.env = env;
        let result = self.exec_statements(stmts).await;
        self.env = saved;
        result
    }

    fn eval<'a>(
        &'a mut self,
        node: &'a Node,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, RuntimeError>> + 'a>>
    {
        Box::pin(self.eval_inner(node))
    }

    async fn eval_inner(&mut self, node: &Node) -> Result<Value, RuntimeError> {
        match node {
            Node::LetBinding { name, value } => {
                let val = self.eval(value).await?;
                self.env.define(name, val, false);
                Ok(Value::Nil)
            }

            Node::VarBinding { name, value } => {
                let val = self.eval(value).await?;
                self.env.define(name, val, true);
                Ok(Value::Nil)
            }

            Node::Assignment { target, value } => {
                let val = self.eval(value).await?;
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
                let cond = self.eval(condition).await?;
                if cond.is_truthy() {
                    self.exec_statements(then_body).await
                } else if let Some(else_body) = else_body {
                    self.exec_statements(else_body).await
                } else {
                    Ok(Value::Nil)
                }
            }

            Node::ForIn {
                variable,
                iterable,
                body,
            } => self.eval_for_in(variable, iterable, body).await,

            Node::MatchExpr { value, arms } => self.eval_match(value, arms).await,
            Node::WhileLoop { condition, body } => self.eval_while(condition, body).await,
            Node::Retry { count, body } => self.eval_retry(count, body).await,

            Node::Parallel {
                count,
                variable,
                body,
            } => self.eval_parallel(count, variable.as_deref(), body).await,

            Node::ParallelMap {
                list,
                variable,
                body,
            } => self.eval_parallel_map(list, variable, body).await,

            Node::ReturnStmt { value } => {
                let val = if let Some(v) = value {
                    Some(self.eval(v).await?)
                } else {
                    None
                };
                Err(RuntimeError::ReturnValue(val))
            }

            Node::FunctionCall { name, args } => self.eval_function_call(name, args).await,

            Node::MethodCall {
                object,
                method,
                args,
            } => self.eval_method_call(object, method, args).await,

            Node::PropertyAccess { object, property } => {
                self.eval_property_access(object, property).await
            }

            Node::SubscriptAccess { object, index } => {
                let obj = self.eval(object).await?;
                let idx = self.eval(index).await?;
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

            Node::BinaryOp { op, left, right } => self.eval_binary_op(op, left, right).await,

            Node::Ternary {
                condition,
                true_expr,
                false_expr,
            } => {
                let cond = self.eval(condition).await?;
                if cond.is_truthy() {
                    self.eval(true_expr).await
                } else {
                    self.eval(false_expr).await
                }
            }

            Node::UnaryOp { op, operand } => {
                let val = self.eval(operand).await?;
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
                let val = self.eval(value).await?;
                Err(RuntimeError::ThrownError(val))
            }

            Node::InterpolatedString(segments) => self.eval_interpolated_string(segments).await,

            Node::StringLiteral(s) => Ok(Value::String(s.clone())),
            Node::IntLiteral(n) => Ok(Value::Int(*n)),
            Node::FloatLiteral(n) => Ok(Value::Float(*n)),
            Node::BoolLiteral(b) => Ok(Value::Bool(*b)),
            Node::NilLiteral => Ok(Value::Nil),
            Node::Identifier(name) => Ok(self.env.get(name).unwrap_or(Value::Nil)),

            Node::ListLiteral(elements) => {
                let mut values = Vec::new();
                for el in elements {
                    values.push(self.eval(el).await?);
                }
                Ok(Value::List(values))
            }

            Node::DictLiteral(entries) => {
                let mut map = BTreeMap::new();
                for entry in entries {
                    let key = self.eval(&entry.key).await?;
                    let val = self.eval(&entry.value).await?;
                    map.insert(key.as_string(), val);
                }
                Ok(Value::Dict(map))
            }

            Node::Block(stmts) => {
                let block_env = self.env.child();
                self.exec_in_env(block_env, stmts).await
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
            } => self.eval_try_catch(body, error_var, catch_body).await,

            Node::FnDecl { name, params, body } => {
                let fn_value = Value::Closure {
                    params: params.clone(),
                    body: body.clone(),
                    env: self.env.clone(),
                };
                self.env.define(name, fn_value, false);
                Ok(Value::Nil)
            }

            Node::SpawnExpr { body } => self.eval_spawn(body),

            Node::ImportDecl { path } => self.eval_import(path).await,

            Node::Pipeline { .. } | Node::OverrideDecl { .. } => Ok(Value::Nil),
        }
    }

    // --- Control flow ---

    async fn eval_for_in(
        &mut self,
        variable: &str,
        iterable: &Node,
        body: &[Node],
    ) -> Result<Value, RuntimeError> {
        let iter_val = self.eval(iterable).await?;

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
            result = self.exec_statements(body).await?;
        }

        self.env = saved;
        Ok(result)
    }

    async fn eval_match(&mut self, value: &Node, arms: &[MatchArm]) -> Result<Value, RuntimeError> {
        let val = self.eval(value).await?;
        for arm in arms {
            let pattern_val = self.eval(&arm.pattern).await?;
            if values_equal(&val, &pattern_val) {
                return self.exec_statements(&arm.body).await;
            }
        }
        Ok(Value::Nil)
    }

    async fn eval_while(&mut self, condition: &Node, body: &[Node]) -> Result<Value, RuntimeError> {
        let mut result = Value::Nil;
        let max_iterations = 10_000;
        let mut iteration = 0;
        while iteration < max_iterations {
            let cond = self.eval(condition).await?;
            if !cond.is_truthy() {
                break;
            }
            let loop_env = self.env.child();
            result = self.exec_in_env(loop_env, body).await?;
            iteration += 1;
        }
        Ok(result)
    }

    async fn eval_retry(
        &mut self,
        count_node: &Node,
        body: &[Node],
    ) -> Result<Value, RuntimeError> {
        let count_val = self.eval(count_node).await?;
        let count = count_val.as_int().unwrap_or(3) as usize;

        for _attempt in 0..count {
            match self.exec_statements(body).await {
                Ok(result) => return Ok(result),
                Err(RuntimeError::ReturnValue(val)) => {
                    return Err(RuntimeError::ReturnValue(val));
                }
                Err(_) => {}
            }
        }
        Ok(Value::Nil)
    }

    async fn eval_try_catch(
        &mut self,
        body: &[Node],
        error_var: &Option<String>,
        catch_body: &[Node],
    ) -> Result<Value, RuntimeError> {
        match self.exec_statements(body).await {
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
                self.exec_in_env(catch_env, catch_body).await
            }
        }
    }

    // --- Concurrency ---

    async fn eval_parallel(
        &mut self,
        count_node: &Node,
        variable: Option<&str>,
        body: &[Node],
    ) -> Result<Value, RuntimeError> {
        let count_val = self.eval(count_node).await?;
        let n = count_val.as_int().unwrap_or(1) as usize;

        let mut handles = Vec::with_capacity(n);
        for i in 0..n {
            let task_env = self.env.child();
            if let Some(var) = variable {
                task_env.define(var, Value::Int(i as i64), false);
            }
            let mut child = self.child_interpreter(task_env);
            let body = body.to_vec();
            handles.push(tokio::task::spawn_local(async move {
                let result = child.exec_statements(&body).await?;
                Ok((result, child.output))
            }));
        }

        let mut results = vec![Value::Nil; n];
        for (i, handle) in handles.into_iter().enumerate() {
            let (value, task_output) = handle
                .await
                .map_err(|e| RuntimeError::thrown(e.to_string()))??;
            results[i] = value;
            self.output.extend(task_output);
        }
        Ok(Value::List(results))
    }

    async fn eval_parallel_map(
        &mut self,
        list_node: &Node,
        variable: &str,
        body: &[Node],
    ) -> Result<Value, RuntimeError> {
        let list_val = self.eval(list_node).await?;
        let items = match list_val {
            Value::List(items) => items,
            _ => return Ok(Value::Nil),
        };

        let n = items.len();
        let mut handles = Vec::with_capacity(n);
        for item in items {
            let task_env = self.env.child();
            task_env.define(variable, item, false);
            let mut child = self.child_interpreter(task_env);
            let body = body.to_vec();
            handles.push(tokio::task::spawn_local(async move {
                let result = child.exec_statements(&body).await?;
                Ok((result, child.output))
            }));
        }

        let mut results = vec![Value::Nil; n];
        for (i, handle) in handles.into_iter().enumerate() {
            let (value, task_output) = handle
                .await
                .map_err(|e| RuntimeError::thrown(e.to_string()))??;
            results[i] = value;
            self.output.extend(task_output);
        }
        Ok(Value::List(results))
    }

    fn eval_spawn(&mut self, body: &[Node]) -> Result<Value, RuntimeError> {
        let task_id = format!("task_{}", TASK_COUNTER.fetch_add(1, Ordering::Relaxed));

        let spawn_env = self.env.child();
        let mut child = self.child_interpreter(spawn_env);
        let body = body.to_vec();

        let handle = tokio::task::spawn_local(async move {
            let result = child.exec_statements(&body).await?;
            Ok((result, child.output))
        });

        self.spawned.lock().unwrap().insert(task_id.clone(), handle);
        Ok(Value::TaskHandle { id: task_id })
    }

    async fn await_task(&mut self, task_id: &str) -> Result<Value, RuntimeError> {
        let handle = self.spawned.lock().unwrap().remove(task_id);
        match handle {
            Some(h) => {
                let (value, task_output) =
                    h.await.map_err(|e| RuntimeError::thrown(e.to_string()))??;
                self.output.extend(task_output);
                Ok(value)
            }
            None => Ok(Value::Nil),
        }
    }

    fn cancel_task(&mut self, task_id: &str) {
        if let Some(h) = self.spawned.lock().unwrap().remove(task_id) {
            h.abort();
        }
    }

    // --- Imports ---

    async fn eval_import(&mut self, path: &str) -> Result<Value, RuntimeError> {
        let resolved = self.source_dir.join(path);
        let resolved = resolved.canonicalize().unwrap_or_else(|_| resolved.clone());

        if self.imported.contains(&resolved) {
            return Ok(Value::Nil);
        }
        self.imported.push(resolved.clone());

        let source = std::fs::read_to_string(&resolved).map_err(|e| RuntimeError::ImportError {
            path: path.to_string(),
            reason: e.to_string(),
        })?;

        let mut lexer = Lexer::new(&source);
        let tokens = lexer
            .tokenize()
            .map_err(|e| RuntimeError::thrown(e.to_string()))?;
        let mut parser = Parser::new(tokens);
        let nodes = parser
            .parse()
            .map_err(|e| RuntimeError::thrown(e.to_string()))?;

        let prev_dir = self.source_dir.clone();
        if let Some(parent) = resolved.parent() {
            self.source_dir = parent.to_path_buf();
        }

        for node in &nodes {
            if let Node::Pipeline { name, .. } = node {
                self.pipelines.insert(name.clone(), node.clone());
            }
        }

        for node in &nodes {
            if !matches!(node, Node::Pipeline { .. }) {
                self.eval(node).await?;
            }
        }

        self.source_dir = prev_dir;
        Ok(Value::Nil)
    }

    // --- Function calls ---

    async fn eval_function_call(
        &mut self,
        name: &str,
        args: &[Node],
    ) -> Result<Value, RuntimeError> {
        // Check for user-defined function (closure) first
        if let Some(Value::Closure { params, body, env }) = self.env.get(name) {
            let arg_values = self.eval_args(args).await?;
            return self.invoke_closure(&params, &body, &env, &arg_values).await;
        }

        // Built-in interpreter functions: await, cancel
        if name == "await" {
            let arg_values = self.eval_args(args).await?;
            if let Some(Value::TaskHandle { id }) = arg_values.first() {
                return self.await_task(id).await;
            }
            return Ok(Value::Nil);
        }
        if name == "cancel" {
            let arg_values = self.eval_args(args).await?;
            if let Some(Value::TaskHandle { id }) = arg_values.first() {
                self.cancel_task(id);
            }
            return Ok(Value::Nil);
        }

        // Check sync builtins
        if let Some(builtin) = self.builtins.get(name).cloned() {
            let arg_values = self.eval_args(args).await?;
            return builtin(&arg_values, &mut self.output);
        }

        // Check async builtins
        if let Some(builtin) = self.async_builtins.get(name).cloned() {
            let arg_values = self.eval_args(args).await?;
            return builtin(arg_values).await;
        }

        Err(RuntimeError::UndefinedBuiltin(name.to_string()))
    }

    async fn eval_args(&mut self, args: &[Node]) -> Result<Vec<Value>, RuntimeError> {
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.eval(arg).await?);
        }
        Ok(values)
    }

    async fn invoke_closure(
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
        let result = self.exec_in_env(call_env, body).await;
        match result {
            Ok(val) => Ok(val),
            Err(RuntimeError::ReturnValue(val)) => Ok(val.unwrap_or(Value::Nil)),
            Err(e) => Err(e),
        }
    }

    /// Helper: invoke a closure with a single item argument.
    #[allow(clippy::cloned_ref_to_slice_refs)]
    async fn invoke_closure_item(
        &mut self,
        closure: (&[String], &[Node], &Environment),
        item: &Value,
    ) -> Result<Value, RuntimeError> {
        let (params, body, env) = closure;
        self.invoke_closure(params, body, env, &[item.clone()])
            .await
    }

    // --- Method calls ---

    async fn eval_method_call(
        &mut self,
        object: &Node,
        method: &str,
        args: &[Node],
    ) -> Result<Value, RuntimeError> {
        let obj = self.eval(object).await?;
        let arg_values = self.eval_args(args).await?;

        if let Node::Identifier(obj_name) = object {
            let qualified = format!("{obj_name}.{method}");
            if let Some(builtin) = self.builtins.get(&qualified).cloned() {
                return builtin(&arg_values, &mut self.output);
            }
            if let Some(builtin) = self.async_builtins.get(&qualified).cloned() {
                return builtin(arg_values).await;
            }
        }

        match obj {
            Value::String(s) => self.eval_string_method(&s, method, &arg_values),
            Value::List(items) => self.eval_list_method(&items, method, &arg_values).await,
            Value::Dict(map) => self.eval_dict_method(&map, method, &arg_values).await,
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

    async fn eval_list_method(
        &mut self,
        items: &[Value],
        method: &str,
        args: &[Value],
    ) -> Result<Value, RuntimeError> {
        match method {
            "count" => Ok(Value::Int(items.len() as i64)),
            "empty" => Ok(Value::Bool(items.is_empty())),
            "map" => {
                let closure = require_closure(args)?;
                let mut results = Vec::new();
                for item in items {
                    results.push(self.invoke_closure_item(closure, item).await?);
                }
                Ok(Value::List(results))
            }
            "filter" => {
                let closure = require_closure(args)?;
                let mut results = Vec::new();
                for item in items {
                    let result = self.invoke_closure_item(closure, item).await?;
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
                            acc = self
                                .invoke_closure(params, body, env, &[acc, item.clone()])
                                .await?;
                        }
                        return Ok(acc);
                    }
                }
                Ok(Value::Nil)
            }
            "find" => {
                let closure = require_closure(args)?;
                for item in items {
                    let result = self.invoke_closure_item(closure, item).await?;
                    if result.is_truthy() {
                        return Ok(item.clone());
                    }
                }
                Ok(Value::Nil)
            }
            "any" => {
                let closure = require_closure(args)?;
                for item in items {
                    let result = self.invoke_closure_item(closure, item).await?;
                    if result.is_truthy() {
                        return Ok(Value::Bool(true));
                    }
                }
                Ok(Value::Bool(false))
            }
            "all" => {
                let closure = require_closure(args)?;
                for item in items {
                    let result = self.invoke_closure_item(closure, item).await?;
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
                    let result = self.invoke_closure_item(closure, item).await?;
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

    // --- Dict methods ---

    async fn eval_dict_method(
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
                    result.insert(k.clone(), self.invoke_closure_item(closure, v).await?);
                }
                Ok(Value::Dict(result))
            }
            "filter" => {
                let closure = require_closure(args)?;
                let mut result = BTreeMap::new();
                for (k, v) in map {
                    let keep = self.invoke_closure_item(closure, v).await?;
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

    async fn eval_property_access(
        &mut self,
        object: &Node,
        property: &str,
    ) -> Result<Value, RuntimeError> {
        let obj = self.eval(object).await?;
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

    async fn eval_binary_op(
        &mut self,
        op: &str,
        left: &Node,
        right: &Node,
    ) -> Result<Value, RuntimeError> {
        if op == "|>" {
            let left_val = self.eval(left).await?;
            let right_val = self.eval(right).await?;
            if let Value::Closure { params, body, env } = &right_val {
                return self.invoke_closure(params, body, env, &[left_val]).await;
            }
            if let Node::Identifier(name) = right {
                if let Some(builtin) = self.builtins.get(name.as_str()).cloned() {
                    return builtin(&[left_val], &mut self.output);
                }
                if let Some(builtin) = self.async_builtins.get(name.as_str()).cloned() {
                    return builtin(vec![left_val]).await;
                }
                if let Some(Value::Closure { params, body, env }) = self.env.get(name) {
                    return self.invoke_closure(&params, &body, &env, &[left_val]).await;
                }
            }
            return Ok(Value::Nil);
        }

        if op == "??" {
            let left_val = self.eval(left).await?;
            if matches!(left_val, Value::Nil) {
                return self.eval(right).await;
            }
            return Ok(left_val);
        }

        if op == "&&" {
            let left_val = self.eval(left).await?;
            if !left_val.is_truthy() {
                return Ok(Value::Bool(false));
            }
            return Ok(Value::Bool(self.eval(right).await?.is_truthy()));
        }

        if op == "||" {
            let left_val = self.eval(left).await?;
            if left_val.is_truthy() {
                return Ok(Value::Bool(true));
            }
            return Ok(Value::Bool(self.eval(right).await?.is_truthy()));
        }

        let left_val = self.eval(left).await?;
        let right_val = self.eval(right).await?;

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
                (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 + b)),
                (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a + *b as f64)),
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
                (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 - b)),
                (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a - *b as f64)),
                _ => Ok(Value::Nil),
            },
            "*" => match (&left_val, &right_val) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_mul(*b))),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
                (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 * b)),
                (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a * *b as f64)),
                _ => Ok(Value::Nil),
            },
            "/" => match (&left_val, &right_val) {
                (Value::Int(a), Value::Int(b)) if *b != 0 => Ok(Value::Int(a / b)),
                (Value::Float(a), Value::Float(b)) if *b != 0.0 => Ok(Value::Float(a / b)),
                (Value::Int(a), Value::Float(b)) if *b != 0.0 => Ok(Value::Float(*a as f64 / b)),
                (Value::Float(a), Value::Int(b)) if *b != 0 => Ok(Value::Float(a / *b as f64)),
                _ => Ok(Value::Nil),
            },
            _ => Ok(Value::Nil),
        }
    }

    // --- Interpolated strings ---

    async fn eval_interpolated_string(
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
                        .map_err(|e| RuntimeError::thrown(e.to_string()))?;
                    let mut parser = Parser::new(tokens);
                    let node = parser
                        .parse_single_expression()
                        .map_err(|e| RuntimeError::thrown(e.to_string()))?;
                    let value = self.eval(&node).await?;
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

fn arg_string(args: &[Value], index: usize) -> String {
    args.get(index).map(|a| a.as_string()).unwrap_or_default()
}

fn require_closure(args: &[Value]) -> Result<(&[String], &[Node], &Environment), RuntimeError> {
    match args.first() {
        Some(Value::Closure { params, body, env }) => Ok((params, body, env)),
        _ => Err(RuntimeError::TypeMismatch {
            expected: "closure".to_string(),
            got: args.first().cloned().unwrap_or(Value::Nil),
        }),
    }
}
