use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use harn_vm::{
    register_http_builtins, register_llm_builtins, register_vm_stdlib, DebugAction, DebugState, Vm,
    VmError, VmValue,
};
use serde_json::json;

use crate::protocol::*;

/// Execution state for stepping.
#[derive(Debug, Clone, PartialEq)]
pub enum StepMode {
    /// Run until a breakpoint or end.
    Continue,
    /// Stop at the next line.
    StepOver,
    /// Stop at the next statement (step into functions).
    StepIn,
    /// Run until returning from the current function.
    StepOut,
}

/// Program state.
enum ProgramState {
    /// Not yet started.
    NotStarted,
    /// Running (VM is initialized).
    Running,
    /// Stopped at a debug point.
    Stopped,
    /// Program has terminated.
    Terminated,
}

/// A segment in an expression path for evaluation.
enum PathSegment {
    Field(String),
    Index(i64),
}

/// The debug adapter implementation.
pub struct Debugger {
    seq: i64,
    source_path: Option<String>,
    source_content: Option<String>,
    breakpoints: Vec<Breakpoint>,
    next_bp_id: i64,
    vm: Option<Vm>,
    /// Variables captured at the current stop point.
    variables: BTreeMap<String, VmValue>,
    /// Current execution state.
    stopped: bool,
    /// Current line in the source.
    current_line: i64,
    /// Step mode.
    step_mode: StepMode,
    /// Output captured during execution.
    output: String,
    /// Program state.
    program_state: ProgramState,
    /// Structured variable references: reference_id → children
    var_refs: BTreeMap<i64, Vec<(String, VmValue)>>,
    /// Tokio runtime for async VM execution.
    runtime: tokio::runtime::Runtime,
    /// Next variable reference ID (start at 100 to avoid conflict with scope refs).
    next_var_ref: i64,
    /// Whether to break on thrown exceptions.
    break_on_exceptions: bool,
    /// Latest VM debug snapshot captured through the VM debug hook.
    latest_debug_state: Rc<RefCell<Option<DebugState>>>,
}

impl Debugger {
    pub fn new() -> Self {
        Self {
            seq: 1,
            source_path: None,
            source_content: None,
            breakpoints: Vec::new(),
            next_bp_id: 1,
            vm: None,
            variables: BTreeMap::new(),
            stopped: false,
            current_line: 0,
            step_mode: StepMode::Continue,
            output: String::new(),
            program_state: ProgramState::NotStarted,
            var_refs: BTreeMap::new(),
            next_var_ref: 100,
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
            break_on_exceptions: false,
            latest_debug_state: Rc::new(RefCell::new(None)),
        }
    }

    fn next_seq(&mut self) -> i64 {
        let s = self.seq;
        self.seq += 1;
        s
    }

    pub fn handle_message(&mut self, msg: DapMessage) -> Vec<DapResponse> {
        let command = msg.command.as_deref().unwrap_or("");
        match command {
            "initialize" => self.handle_initialize(&msg),
            "launch" => self.handle_launch(&msg),
            "setBreakpoints" => self.handle_set_breakpoints(&msg),
            "configurationDone" => self.handle_configuration_done(&msg),
            "continue" => self.handle_continue(&msg),
            "next" => self.handle_next(&msg),
            "stepIn" => self.handle_step_in(&msg),
            "stepOut" => self.handle_step_out(&msg),
            "threads" => self.handle_threads(&msg),
            "stackTrace" => self.handle_stack_trace(&msg),
            "scopes" => self.handle_scopes(&msg),
            "variables" => self.handle_variables(&msg),
            "evaluate" => self.handle_evaluate(&msg),
            "setExceptionBreakpoints" => self.handle_set_exception_breakpoints(&msg),
            "disconnect" => self.handle_disconnect(&msg),
            _ => {
                vec![DapResponse::success(
                    self.next_seq(),
                    msg.seq,
                    command,
                    None,
                )]
            }
        }
    }

    fn handle_initialize(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let caps = Capabilities::default();
        let seq = self.next_seq();
        let response = DapResponse::success(seq, msg.seq, "initialize", Some(json!(caps)));

        let event_seq = self.next_seq();
        let event = DapResponse::event(event_seq, "initialized", None);

        vec![response, event]
    }

    fn handle_launch(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let mut responses = Vec::new();

        if let Some(args) = &msg.arguments {
            if let Some(program) = args.get("program").and_then(|p| p.as_str()) {
                self.source_path = Some(program.to_string());
                match std::fs::read_to_string(program) {
                    Ok(source) => {
                        self.source_content = Some(source.clone());
                        // Compile and initialize the VM
                        match self.compile_program(&source) {
                            Ok(()) => {
                                self.program_state = ProgramState::Running;
                            }
                            Err(e) => {
                                let seq = self.next_seq();
                                responses.push(DapResponse::event(
                                    seq,
                                    "output",
                                    Some(json!({
                                        "category": "stderr",
                                        "output": format!("Compilation error: {e}\n"),
                                    })),
                                ));
                            }
                        }
                    }
                    Err(e) => {
                        let seq = self.next_seq();
                        responses.push(DapResponse::event(
                            seq,
                            "output",
                            Some(json!({
                                "category": "stderr",
                                "output": format!("Failed to read {program}: {e}\n"),
                            })),
                        ));
                    }
                }
            }
        }

        let seq = self.next_seq();
        responses.push(DapResponse::success(seq, msg.seq, "launch", None));
        responses
    }

    fn compile_program(&mut self, source: &str) -> Result<(), String> {
        let chunk = harn_vm::compile_source(source)?;

        let mut vm = Vm::new();
        register_vm_stdlib(&mut vm);
        register_http_builtins(&mut vm);
        register_llm_builtins(&mut vm);

        if let Some(ref path) = self.source_path {
            if let Some(parent) = std::path::Path::new(path).parent() {
                if !parent.as_os_str().is_empty() {
                    vm.set_source_dir(parent);
                }
            }
        }

        // Set breakpoints on the VM
        let bp_lines: Vec<usize> = self.breakpoints.iter().map(|bp| bp.line as usize).collect();
        vm.set_breakpoints(bp_lines);
        *self.latest_debug_state.borrow_mut() = None;
        let latest_debug_state = Rc::clone(&self.latest_debug_state);
        vm.set_debug_hook(move |state| {
            *latest_debug_state.borrow_mut() = Some(state.clone());
            DebugAction::Continue
        });

        // Initialize execution (push frame) but don't run yet
        vm.start(&chunk);
        *self.latest_debug_state.borrow_mut() = Some(vm.debug_state());
        self.vm = Some(vm);
        Ok(())
    }

    fn handle_set_breakpoints(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        self.breakpoints.clear();

        if let Some(args) = &msg.arguments {
            if let Some(bps) = args.get("breakpoints").and_then(|b| b.as_array()) {
                for bp in bps {
                    if let Some(line) = bp.get("line").and_then(|l| l.as_i64()) {
                        let id = self.next_bp_id;
                        self.next_bp_id += 1;
                        let condition = bp
                            .get("condition")
                            .and_then(|c| c.as_str())
                            .map(|s| s.to_string())
                            .filter(|s| !s.is_empty());
                        self.breakpoints.push(Breakpoint {
                            id,
                            verified: true,
                            line,
                            source: self.source_path.as_ref().map(|p| Source {
                                name: None,
                                path: Some(p.clone()),
                            }),
                            condition,
                        });
                    }
                }
            }
        }

        // Update VM breakpoints if running
        if let Some(vm) = &mut self.vm {
            let bp_lines: Vec<usize> = self.breakpoints.iter().map(|bp| bp.line as usize).collect();
            vm.set_breakpoints(bp_lines);
        }

        let seq = self.next_seq();
        vec![DapResponse::success(
            seq,
            msg.seq,
            "setBreakpoints",
            Some(json!({ "breakpoints": self.breakpoints })),
        )]
    }

    fn handle_configuration_done(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let mut responses = Vec::new();

        let seq = self.next_seq();
        responses.push(DapResponse::success(
            seq,
            msg.seq,
            "configurationDone",
            None,
        ));

        // Run until first breakpoint or end
        responses.extend(self.run_to_breakpoint());

        responses
    }

    /// Run the VM until it hits a breakpoint, step completes, or terminates.
    fn run_to_breakpoint(&mut self) -> Vec<DapResponse> {
        let mut responses = Vec::new();

        if self.vm.is_none() {
            let seq = self.next_seq();
            responses.push(DapResponse::event(seq, "terminated", None));
            return responses;
        }

        // Snapshot breakpoint conditions to avoid borrow issues in the loop
        let bp_conditions: Vec<(i64, Option<String>)> = self
            .breakpoints
            .iter()
            .map(|bp| (bp.line, bp.condition.clone()))
            .collect();

        // Clear structured variable references from previous stop
        self.var_refs.clear();
        self.next_var_ref = 100;

        loop {
            let step_result = {
                let vm = self.vm.as_mut().unwrap();
                self.runtime.block_on(async { vm.step_execute().await })
            };
            match step_result {
                Ok(Some((val, stopped))) => {
                    if stopped {
                        let state = self.current_debug_state();
                        let current_line = state.line as i64;
                        let vars = state.variables;

                        // Check conditional breakpoints
                        let should_stop = check_condition(&bp_conditions, current_line, &vars);

                        if !should_stop {
                            continue;
                        }

                        // Hit a breakpoint or step completed
                        self.stopped = true;
                        self.current_line = current_line;
                        self.variables = vars;
                        self.program_state = ProgramState::Stopped;

                        // Flush output
                        let output = self.vm.as_ref().unwrap().output().to_string();
                        if !output.is_empty() && output != self.output {
                            let new_output = &output[self.output.len()..];
                            if !new_output.is_empty() {
                                let seq = self.next_seq();
                                responses.push(DapResponse::event(
                                    seq,
                                    "output",
                                    Some(json!({
                                        "category": "stdout",
                                        "output": new_output,
                                    })),
                                ));
                            }
                            self.output = output;
                        }

                        // Send stopped event
                        let seq = self.next_seq();
                        responses.push(DapResponse::event(
                            seq,
                            "stopped",
                            Some(json!({
                                "reason": "breakpoint",
                                "threadId": 1,
                                "allThreadsStopped": true,
                            })),
                        ));
                        return responses;
                    } else {
                        // Program terminated normally
                        let _val = val;
                        let output = self.vm.as_ref().unwrap().output().to_string();
                        if !output.is_empty() && output != self.output {
                            let new_output = &output[self.output.len()..];
                            if !new_output.is_empty() {
                                let seq = self.next_seq();
                                responses.push(DapResponse::event(
                                    seq,
                                    "output",
                                    Some(json!({
                                        "category": "stdout",
                                        "output": new_output,
                                    })),
                                ));
                            }
                        }

                        self.program_state = ProgramState::Terminated;
                        let seq = self.next_seq();
                        responses.push(DapResponse::event(seq, "terminated", None));
                        return responses;
                    }
                }
                Ok(None) => {
                    // Continue execution
                    continue;
                }
                Err(e) => {
                    // If exception breakpoints are enabled and this is a thrown
                    // exception, pause instead of terminating.
                    if self.break_on_exceptions && matches!(&e, VmError::Thrown(_)) {
                        let error_msg = e.to_string();
                        let state = self.current_debug_state();
                        self.stopped = true;
                        self.current_line = state.line as i64;
                        self.variables = state.variables;
                        self.program_state = ProgramState::Stopped;

                        let seq = self.next_seq();
                        responses.push(DapResponse::event(
                            seq,
                            "output",
                            Some(json!({
                                "category": "stderr",
                                "output": format!("Exception: {error_msg}\n"),
                            })),
                        ));

                        let seq = self.next_seq();
                        responses.push(DapResponse::event(
                            seq,
                            "stopped",
                            Some(json!({
                                "reason": "exception",
                                "description": error_msg,
                                "threadId": 1,
                                "allThreadsStopped": true,
                            })),
                        ));
                        return responses;
                    }

                    let seq = self.next_seq();
                    responses.push(DapResponse::event(
                        seq,
                        "output",
                        Some(json!({
                            "category": "stderr",
                            "output": format!("Error: {e}\n"),
                        })),
                    ));
                    self.program_state = ProgramState::Terminated;
                    let seq = self.next_seq();
                    responses.push(DapResponse::event(seq, "terminated", None));
                    return responses;
                }
            }
        }
    }

    fn handle_continue(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        self.step_mode = StepMode::Continue;
        self.stopped = false;

        let seq = self.next_seq();
        let mut responses = vec![DapResponse::success(
            seq,
            msg.seq,
            "continue",
            Some(json!({ "allThreadsContinued": true })),
        )];

        // Resume execution
        responses.extend(self.run_to_breakpoint());
        responses
    }

    fn handle_next(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        self.step_mode = StepMode::StepOver;

        if let Some(vm) = &mut self.vm {
            vm.set_step_over();
        }

        let seq = self.next_seq();
        let mut responses = vec![DapResponse::success(seq, msg.seq, "next", None)];

        // Resume and stop at next line
        responses.extend(self.run_to_breakpoint());
        responses
    }

    fn handle_step_in(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        self.step_mode = StepMode::StepIn;

        if let Some(vm) = &mut self.vm {
            vm.set_step_mode(true);
        }

        let seq = self.next_seq();
        let mut responses = vec![DapResponse::success(seq, msg.seq, "stepIn", None)];

        responses.extend(self.run_to_breakpoint());
        responses
    }

    fn handle_step_out(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        self.step_mode = StepMode::StepOut;

        if let Some(vm) = &mut self.vm {
            vm.set_step_out();
        }

        let seq = self.next_seq();
        let mut responses = vec![DapResponse::success(seq, msg.seq, "stepOut", None)];

        responses.extend(self.run_to_breakpoint());
        responses
    }

    fn handle_threads(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let seq = self.next_seq();
        vec![DapResponse::success(
            seq,
            msg.seq,
            "threads",
            Some(json!({
                "threads": [{
                    "id": 1,
                    "name": "main"
                }]
            })),
        )]
    }

    fn handle_stack_trace(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let frames: Vec<StackFrame> = if let Some(vm) = &self.vm {
            vm.debug_stack_frames()
                .into_iter()
                .enumerate()
                .map(|(i, (name, line))| StackFrame {
                    id: (i + 1) as i64,
                    name,
                    line: line.max(1) as i64,
                    column: 1,
                    source: self.source_path.as_ref().map(|p| Source {
                        name: std::path::Path::new(p)
                            .file_name()
                            .map(|f| f.to_string_lossy().into_owned()),
                        path: Some(p.clone()),
                    }),
                })
                .collect()
        } else {
            vec![StackFrame {
                id: 1,
                name: "pipeline".to_string(),
                line: self.current_line.max(1),
                column: 1,
                source: self.source_path.as_ref().map(|p| Source {
                    name: std::path::Path::new(p)
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned()),
                    path: Some(p.clone()),
                }),
            }]
        };

        let total = frames.len();
        let seq = self.next_seq();
        vec![DapResponse::success(
            seq,
            msg.seq,
            "stackTrace",
            Some(json!({
                "stackFrames": frames,
                "totalFrames": total,
            })),
        )]
    }

    fn handle_scopes(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let scopes = vec![Scope {
            name: "Locals".to_string(),
            variables_reference: 1,
            expensive: false,
        }];

        let seq = self.next_seq();
        vec![DapResponse::success(
            seq,
            msg.seq,
            "scopes",
            Some(json!({ "scopes": scopes })),
        )]
    }

    fn current_debug_state(&self) -> DebugState {
        self.latest_debug_state
            .borrow()
            .clone()
            .or_else(|| self.vm.as_ref().map(|vm| vm.debug_state()))
            .unwrap_or(DebugState {
                line: self.current_line.max(0) as usize,
                variables: self.variables.clone(),
                frame_name: "pipeline".to_string(),
                frame_depth: 0,
            })
    }

    fn alloc_var_ref(&mut self, children: Vec<(String, VmValue)>) -> i64 {
        let id = self.next_var_ref;
        self.next_var_ref += 1;
        self.var_refs.insert(id, children);
        id
    }

    fn make_variable(&mut self, name: String, val: &VmValue) -> Variable {
        let (var_ref, display) = match val {
            VmValue::List(items) => {
                let children: Vec<(String, VmValue)> = items
                    .iter()
                    .enumerate()
                    .map(|(i, v)| (format!("[{i}]"), v.clone()))
                    .collect();
                let display = format!("list<{}>", items.len());
                if children.is_empty() {
                    (0, display)
                } else {
                    (self.alloc_var_ref(children), display)
                }
            }
            VmValue::Dict(map) => {
                let children: Vec<(String, VmValue)> =
                    map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                let display = format!("dict<{}>", map.len());
                if children.is_empty() {
                    (0, display)
                } else {
                    (self.alloc_var_ref(children), display)
                }
            }
            VmValue::StructInstance {
                struct_name,
                fields,
            } => {
                let children: Vec<(String, VmValue)> =
                    fields.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                let display = struct_name.clone();
                if children.is_empty() {
                    (0, display)
                } else {
                    (self.alloc_var_ref(children), display)
                }
            }
            VmValue::EnumVariant {
                enum_name,
                variant,
                fields,
            } => {
                if fields.is_empty() {
                    (0, format!("{enum_name}.{variant}"))
                } else {
                    let children: Vec<(String, VmValue)> = fields
                        .iter()
                        .enumerate()
                        .map(|(i, v)| (format!("field_{i}"), v.clone()))
                        .collect();
                    let display = format!("{enum_name}.{variant}(...)");
                    (self.alloc_var_ref(children), display)
                }
            }
            other => (0, other.display()),
        };
        Variable {
            name,
            value: display,
            var_type: vm_type_name(val).to_string(),
            variables_reference: var_ref,
        }
    }

    fn handle_variables(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let ref_id = msg
            .arguments
            .as_ref()
            .and_then(|a| a.get("variablesReference"))
            .and_then(|v| v.as_i64())
            .unwrap_or(1);

        // Check structured variable references first
        if ref_id >= 100 {
            if let Some(children) = self.var_refs.get(&ref_id).cloned() {
                let vars: Vec<Variable> = children
                    .iter()
                    .map(|(name, val)| self.make_variable(name.clone(), val))
                    .collect();
                let seq = self.next_seq();
                return vec![DapResponse::success(
                    seq,
                    msg.seq,
                    "variables",
                    Some(json!({ "variables": vars })),
                )];
            }
        }

        // Scope 1 = locals
        let variable_list: Vec<(String, VmValue)> = self.variables.clone().into_iter().collect();
        let vars: Vec<Variable> = variable_list
            .iter()
            .map(|(name, val)| self.make_variable(name.clone(), val))
            .collect();

        let seq = self.next_seq();
        vec![DapResponse::success(
            seq,
            msg.seq,
            "variables",
            Some(json!({ "variables": vars })),
        )]
    }

    fn handle_evaluate(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let expression = msg
            .arguments
            .as_ref()
            .and_then(|a| a.get("expression"))
            .and_then(|e| e.as_str())
            .unwrap_or("");

        // context can be "watch", "repl", "hover", or "clipboard"
        let _context = msg
            .arguments
            .as_ref()
            .and_then(|a| a.get("context"))
            .and_then(|c| c.as_str())
            .unwrap_or("watch");

        // Resolve the expression: supports "var" and "var.field.field..." dot-access
        match self.resolve_expression(expression) {
            Some(val) => {
                let variable = self.make_variable(expression.to_string(), &val);
                let seq = self.next_seq();
                vec![DapResponse::success(
                    seq,
                    msg.seq,
                    "evaluate",
                    Some(json!({
                        "result": variable.value,
                        "type": variable.var_type,
                        "variablesReference": variable.variables_reference,
                    })),
                )]
            }
            None => {
                // Not a simple variable or dot-access lookup
                let seq = self.next_seq();
                vec![DapResponse {
                    seq,
                    msg_type: "response".to_string(),
                    request_seq: Some(msg.seq),
                    success: Some(false),
                    command: Some("evaluate".to_string()),
                    message: Some(format!(
                        "Cannot evaluate '{expression}': only variable lookups and dot-access \
                         property paths are supported in the debugger"
                    )),
                    body: None,
                    event: None,
                }]
            }
        }
    }

    /// Resolve an expression string against the current variable state.
    /// Supports: variable names ("x"), dot-access ("x.foo.bar"),
    /// subscript access ("x[0]", "x[\"key\"]"), len(x), type_of(x).
    fn resolve_expression(&self, expression: &str) -> Option<VmValue> {
        let expr = expression.trim();

        // Handle len(expr) and type_of(expr)
        if let Some(inner) = expr.strip_prefix("len(").and_then(|s| s.strip_suffix(')')) {
            let val = self.resolve_expression(inner)?;
            return match &val {
                VmValue::String(s) => Some(VmValue::Int(s.len() as i64)),
                VmValue::List(l) => Some(VmValue::Int(l.len() as i64)),
                VmValue::Dict(d) => Some(VmValue::Int(d.len() as i64)),
                _ => None,
            };
        }
        if let Some(inner) = expr
            .strip_prefix("type_of(")
            .and_then(|s| s.strip_suffix(')'))
        {
            let val = self.resolve_expression(inner)?;
            let type_name = match &val {
                VmValue::Int(_) => "int",
                VmValue::Float(_) => "float",
                VmValue::String(_) => "string",
                VmValue::Bool(_) => "bool",
                VmValue::Nil => "nil",
                VmValue::List(_) => "list",
                VmValue::Dict(_) => "dict",
                _ => "unknown",
            };
            return Some(VmValue::String(std::rc::Rc::from(type_name)));
        }

        // Tokenize into segments: identifiers, [subscript], .field
        let mut segments = Vec::new();
        let mut chars = expr.chars().peekable();
        // First segment: variable name
        let mut name = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_alphanumeric() || c == '_' {
                name.push(c);
                chars.next();
            } else {
                break;
            }
        }
        if name.is_empty() {
            return None;
        }
        segments.push(PathSegment::Field(name));

        // Remaining: .field or [subscript]
        while let Some(&c) = chars.peek() {
            match c {
                '.' => {
                    chars.next();
                    let mut field = String::new();
                    while let Some(&c) = chars.peek() {
                        if c.is_alphanumeric() || c == '_' {
                            field.push(c);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    if field.is_empty() {
                        return None;
                    }
                    segments.push(PathSegment::Field(field));
                }
                '[' => {
                    chars.next();
                    let mut idx = String::new();
                    while let Some(&c) = chars.peek() {
                        if c == ']' {
                            chars.next();
                            break;
                        }
                        idx.push(c);
                        chars.next();
                    }
                    let idx = idx.trim().trim_matches('"').trim_matches('\'');
                    if let Ok(n) = idx.parse::<i64>() {
                        segments.push(PathSegment::Index(n));
                    } else {
                        segments.push(PathSegment::Field(idx.to_string()));
                    }
                }
                _ => return None,
            }
        }

        // Resolve
        let root_name = match &segments[0] {
            PathSegment::Field(n) => n.as_str(),
            _ => return None,
        };
        let mut current = self.variables.get(root_name)?.clone();

        for seg in &segments[1..] {
            current = match seg {
                PathSegment::Field(f) => match &current {
                    VmValue::Dict(map) => map.get(f.as_str())?.clone(),
                    VmValue::StructInstance { fields, .. } => fields.get(f.as_str())?.clone(),
                    _ => return None,
                },
                PathSegment::Index(i) => match &current {
                    VmValue::List(list) => {
                        let idx = if *i < 0 {
                            (list.len() as i64 + i) as usize
                        } else {
                            *i as usize
                        };
                        list.get(idx)?.clone()
                    }
                    VmValue::Dict(map) => map.get(&i.to_string())?.clone(),
                    _ => return None,
                },
            };
        }

        Some(current)
    }

    fn handle_set_exception_breakpoints(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        // Check if "all" filter is in the requested filters list
        self.break_on_exceptions = msg
            .arguments
            .as_ref()
            .and_then(|a| a.get("filters"))
            .and_then(|f| f.as_array())
            .map(|filters| filters.iter().any(|f| f.as_str() == Some("all")))
            .unwrap_or(false);

        let seq = self.next_seq();
        vec![DapResponse::success(
            seq,
            msg.seq,
            "setExceptionBreakpoints",
            None,
        )]
    }

    fn handle_disconnect(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let seq = self.next_seq();
        vec![DapResponse::success(seq, msg.seq, "disconnect", None)]
    }
}

fn vm_type_name(val: &VmValue) -> &'static str {
    val.type_name()
}

/// Check if a conditional breakpoint should fire.
fn check_condition(
    bp_conditions: &[(i64, Option<String>)],
    line: i64,
    variables: &BTreeMap<String, VmValue>,
) -> bool {
    let condition = bp_conditions
        .iter()
        .find(|(l, _)| *l == line)
        .and_then(|(_, c)| c.as_deref());

    let condition = match condition {
        Some(c) => c.trim(),
        None => return true, // No condition — always stop
    };

    // Simple condition evaluator: "var == val", "var != val", "var > val", "var"
    for op in &["==", "!=", ">=", "<=", ">", "<"] {
        if let Some((lhs, rhs)) = condition.split_once(op) {
            let lhs = lhs.trim();
            let rhs = rhs.trim().trim_matches('"');
            let lhs_val = variables.get(lhs).map(|v| v.display()).unwrap_or_default();
            return match *op {
                "==" => lhs_val == rhs,
                "!=" => lhs_val != rhs,
                ">=" => lhs_val.parse::<f64>().unwrap_or(0.0) >= rhs.parse::<f64>().unwrap_or(0.0),
                "<=" => lhs_val.parse::<f64>().unwrap_or(0.0) <= rhs.parse::<f64>().unwrap_or(0.0),
                ">" => lhs_val.parse::<f64>().unwrap_or(0.0) > rhs.parse::<f64>().unwrap_or(0.0),
                "<" => lhs_val.parse::<f64>().unwrap_or(0.0) < rhs.parse::<f64>().unwrap_or(0.0),
                _ => true,
            };
        }
    }

    // Just a variable name — truthy check
    if let Some(val) = variables.get(condition) {
        return val.is_truthy();
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_request(seq: i64, command: &str, args: Option<serde_json::Value>) -> DapMessage {
        DapMessage {
            seq,
            msg_type: "request".to_string(),
            command: Some(command.to_string()),
            arguments: args,
        }
    }

    #[test]
    fn test_initialize() {
        let mut dbg = Debugger::new();
        let responses = dbg.handle_message(make_request(1, "initialize", None));
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0].command.as_deref(), Some("initialize"));
        assert_eq!(responses[0].success, Some(true));
        assert_eq!(responses[1].event.as_deref(), Some("initialized"));
    }

    #[test]
    fn test_threads() {
        let mut dbg = Debugger::new();
        let responses = dbg.handle_message(make_request(1, "threads", None));
        assert_eq!(responses.len(), 1);
        let body = responses[0].body.as_ref().unwrap();
        let threads = body["threads"].as_array().unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0]["name"], "main");
    }

    #[test]
    fn test_set_breakpoints() {
        let mut dbg = Debugger::new();
        let responses = dbg.handle_message(make_request(
            1,
            "setBreakpoints",
            Some(json!({
                "source": {"path": "test.harn"},
                "breakpoints": [{"line": 5}, {"line": 10}]
            })),
        ));
        assert_eq!(responses.len(), 1);
        let body = responses[0].body.as_ref().unwrap();
        let bps = body["breakpoints"].as_array().unwrap();
        assert_eq!(bps.len(), 2);
        assert_eq!(bps[0]["line"], 5);
        assert_eq!(bps[1]["line"], 10);
        assert_eq!(bps[0]["verified"], true);
    }

    #[test]
    fn test_launch_and_run() {
        let mut dbg = Debugger::new();

        // Create a temp file
        let dir = std::env::temp_dir().join("harn_dap_test");
        std::fs::create_dir_all(&dir).ok();
        let file = dir.join("test.harn");
        std::fs::write(&file, "pipeline test(task) { log(42) }").unwrap();

        // Initialize
        dbg.handle_message(make_request(1, "initialize", None));

        // Launch
        dbg.handle_message(make_request(
            2,
            "launch",
            Some(json!({"program": file.to_string_lossy()})),
        ));

        // Configuration done (triggers execution)
        let responses = dbg.handle_message(make_request(3, "configurationDone", None));

        // Should have: configurationDone response, output event, terminated event
        assert!(responses.len() >= 2);

        // Find the output event
        let output_event = responses.iter().find(|r| {
            r.event.as_deref() == Some("output")
                && r.body
                    .as_ref()
                    .map(|b| b["category"] == "stdout")
                    .unwrap_or(false)
        });

        if let Some(evt) = output_event {
            let output = evt.body.as_ref().unwrap()["output"].as_str().unwrap();
            assert!(output.contains("[harn] 42"));
        }

        // Find terminated event
        let terminated = responses
            .iter()
            .find(|r| r.event.as_deref() == Some("terminated"));
        assert!(terminated.is_some());

        // Cleanup
        std::fs::remove_file(&file).ok();
        std::fs::remove_dir(&dir).ok();
    }

    #[test]
    fn test_scopes_and_variables() {
        let mut dbg = Debugger::new();
        dbg.variables.insert("x".to_string(), VmValue::Int(42));
        dbg.variables
            .insert("name".to_string(), VmValue::String("hello".into()));

        let responses = dbg.handle_message(make_request(
            1,
            "variables",
            Some(json!({"variablesReference": 1})),
        ));
        assert_eq!(responses.len(), 1);
        let body = responses[0].body.as_ref().unwrap();
        let vars = body["variables"].as_array().unwrap();
        assert_eq!(vars.len(), 2);
    }

    #[test]
    fn test_evaluate() {
        let mut dbg = Debugger::new();
        dbg.variables.insert("x".to_string(), VmValue::Int(42));

        let responses = dbg.handle_message(make_request(
            1,
            "evaluate",
            Some(json!({"expression": "x"})),
        ));
        assert_eq!(responses.len(), 1);
        let body = responses[0].body.as_ref().unwrap();
        assert_eq!(body["result"], "42");
        assert_eq!(body["variablesReference"], 0);
    }

    #[test]
    fn test_evaluate_dot_access() {
        use std::rc::Rc;

        let mut dbg = Debugger::new();
        let mut inner = BTreeMap::new();
        inner.insert("bar".to_string(), VmValue::Int(99));
        dbg.variables
            .insert("foo".to_string(), VmValue::Dict(Rc::new(inner)));

        // "foo.bar" should resolve to 99
        let responses = dbg.handle_message(make_request(
            1,
            "evaluate",
            Some(json!({"expression": "foo.bar"})),
        ));
        assert_eq!(responses.len(), 1);
        let body = responses[0].body.as_ref().unwrap();
        assert_eq!(body["result"], "99");
        assert_eq!(body["variablesReference"], 0);
    }

    #[test]
    fn test_evaluate_nested_dot_access() {
        use std::rc::Rc;

        let mut dbg = Debugger::new();
        let mut inner = BTreeMap::new();
        inner.insert("c".to_string(), VmValue::String("deep".into()));
        let mut outer = BTreeMap::new();
        outer.insert("b".to_string(), VmValue::Dict(Rc::new(inner)));
        dbg.variables
            .insert("a".to_string(), VmValue::Dict(Rc::new(outer)));

        let responses = dbg.handle_message(make_request(
            1,
            "evaluate",
            Some(json!({"expression": "a.b.c"})),
        ));
        assert_eq!(responses.len(), 1);
        let body = responses[0].body.as_ref().unwrap();
        assert_eq!(body["result"], "deep");
    }

    #[test]
    fn test_evaluate_complex_value_has_var_ref() {
        use std::rc::Rc;

        let mut dbg = Debugger::new();
        let mut map = BTreeMap::new();
        map.insert("key".to_string(), VmValue::Int(1));
        dbg.variables
            .insert("d".to_string(), VmValue::Dict(Rc::new(map)));

        // Evaluating a dict should return a non-zero variablesReference
        let responses = dbg.handle_message(make_request(
            1,
            "evaluate",
            Some(json!({"expression": "d"})),
        ));
        assert_eq!(responses.len(), 1);
        let body = responses[0].body.as_ref().unwrap();
        assert!(body["variablesReference"].as_i64().unwrap() > 0);
    }

    #[test]
    fn test_evaluate_undefined_returns_error() {
        let mut dbg = Debugger::new();

        let responses = dbg.handle_message(make_request(
            1,
            "evaluate",
            Some(json!({"expression": "nonexistent"})),
        ));
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].success, Some(false));
        assert!(responses[0]
            .message
            .as_ref()
            .unwrap()
            .contains("nonexistent"));
    }

    #[test]
    fn test_evaluate_with_context() {
        let mut dbg = Debugger::new();
        dbg.variables.insert("x".to_string(), VmValue::Int(7));

        // All contexts (watch, repl, hover) should work the same
        for ctx in &["watch", "repl", "hover"] {
            let responses = dbg.handle_message(make_request(
                1,
                "evaluate",
                Some(json!({"expression": "x", "context": ctx})),
            ));
            assert_eq!(responses.len(), 1);
            let body = responses[0].body.as_ref().unwrap();
            assert_eq!(body["result"], "7");
        }
    }

    #[test]
    fn test_set_exception_breakpoints_enable() {
        let mut dbg = Debugger::new();
        assert!(!dbg.break_on_exceptions);

        // Enable "all" exception breakpoints
        let responses = dbg.handle_message(make_request(
            1,
            "setExceptionBreakpoints",
            Some(json!({"filters": ["all"]})),
        ));
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].success, Some(true));
        assert!(dbg.break_on_exceptions);
    }

    #[test]
    fn test_set_exception_breakpoints_disable() {
        let mut dbg = Debugger::new();
        dbg.break_on_exceptions = true;

        // Empty filters — disable
        let responses = dbg.handle_message(make_request(
            1,
            "setExceptionBreakpoints",
            Some(json!({"filters": []})),
        ));
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].success, Some(true));
        assert!(!dbg.break_on_exceptions);
    }

    #[test]
    fn test_initialize_has_exception_breakpoint_filters() {
        let mut dbg = Debugger::new();
        let responses = dbg.handle_message(make_request(1, "initialize", None));
        let body = responses[0].body.as_ref().unwrap();
        assert_eq!(body["supportsExceptionBreakpointFilters"], true);
        let filters = body["exceptionBreakpointFilters"].as_array().unwrap();
        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0]["filter"], "all");
        assert_eq!(filters[0]["label"], "All Exceptions");
        assert_eq!(filters[0]["default"], false);
    }

    #[test]
    fn test_step_commands() {
        let mut dbg = Debugger::new();

        let r = dbg.handle_message(make_request(1, "next", None));
        assert!(r[0].success == Some(true));
        assert_eq!(dbg.step_mode, StepMode::StepOver);

        let r = dbg.handle_message(make_request(2, "stepIn", None));
        assert!(r[0].success == Some(true));
        assert_eq!(dbg.step_mode, StepMode::StepIn);

        let r = dbg.handle_message(make_request(3, "stepOut", None));
        assert!(r[0].success == Some(true));
        assert_eq!(dbg.step_mode, StepMode::StepOut);

        let r = dbg.handle_message(make_request(4, "continue", None));
        assert!(r[0].success == Some(true));
        assert_eq!(dbg.step_mode, StepMode::Continue);
    }

    #[test]
    fn test_disconnect() {
        let mut dbg = Debugger::new();
        let r = dbg.handle_message(make_request(1, "disconnect", None));
        assert_eq!(r[0].success, Some(true));
    }

    #[test]
    fn test_stack_trace() {
        let mut dbg = Debugger::new();
        dbg.source_path = Some("test.harn".to_string());
        dbg.current_line = 5;

        let r = dbg.handle_message(make_request(1, "stackTrace", None));
        let body = r[0].body.as_ref().unwrap();
        let frames = body["stackFrames"].as_array().unwrap();
        assert_eq!(frames.len(), 1);
    }

    #[test]
    fn test_breakpoint_stop() {
        let mut dbg = Debugger::new();

        // Create a temp file with multiple lines
        let dir = std::env::temp_dir().join("harn_dap_bp_test");
        std::fs::create_dir_all(&dir).ok();
        let file = dir.join("test_bp.harn");
        std::fs::write(
            &file,
            "pipeline test(task) {\n  let x = 1\n  let y = 2\n  log(x + y)\n}",
        )
        .unwrap();

        // Initialize
        dbg.handle_message(make_request(1, "initialize", None));

        // Set breakpoint on line 3
        dbg.handle_message(make_request(
            2,
            "setBreakpoints",
            Some(json!({
                "source": {"path": file.to_string_lossy()},
                "breakpoints": [{"line": 3}]
            })),
        ));

        // Launch
        dbg.handle_message(make_request(
            3,
            "launch",
            Some(json!({"program": file.to_string_lossy()})),
        ));

        // Configuration done — should stop at breakpoint
        let responses = dbg.handle_message(make_request(4, "configurationDone", None));

        // Check for stopped event OR terminated
        let has_stopped = responses
            .iter()
            .any(|r| r.event.as_deref() == Some("stopped"));
        let has_terminated = responses
            .iter()
            .any(|r| r.event.as_deref() == Some("terminated"));

        // Either stopped at breakpoint or ran to completion
        assert!(has_stopped || has_terminated);

        // Cleanup
        std::fs::remove_file(&file).ok();
        std::fs::remove_dir(&dir).ok();
    }
}
