use std::collections::BTreeMap;

use harn_lexer::Lexer;
use harn_parser::Parser;
use harn_vm::{register_vm_stdlib, Compiler, Vm, VmValue};
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
    #[allow(dead_code)]
    output: String,
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
                        self.source_content = Some(source);
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

    fn handle_set_breakpoints(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        self.breakpoints.clear();

        if let Some(args) = &msg.arguments {
            if let Some(bps) = args.get("breakpoints").and_then(|b| b.as_array()) {
                for bp in bps {
                    if let Some(line) = bp.get("line").and_then(|l| l.as_i64()) {
                        let id = self.next_bp_id;
                        self.next_bp_id += 1;
                        self.breakpoints.push(Breakpoint {
                            id,
                            verified: true,
                            line,
                            source: self.source_path.as_ref().map(|p| Source {
                                name: None,
                                path: Some(p.clone()),
                            }),
                        });
                    }
                }
            }
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

        // Execute the program
        let seq = self.next_seq();
        responses.push(DapResponse::success(
            seq,
            msg.seq,
            "configurationDone",
            None,
        ));

        // Run the program
        if let Some(source) = &self.source_content {
            let result = self.run_program(source.clone());
            match result {
                Ok(output) => {
                    if !output.is_empty() {
                        let seq = self.next_seq();
                        responses.push(DapResponse::event(
                            seq,
                            "output",
                            Some(json!({
                                "category": "stdout",
                                "output": output,
                            })),
                        ));
                    }
                }
                Err(e) => {
                    let seq = self.next_seq();
                    responses.push(DapResponse::event(
                        seq,
                        "output",
                        Some(json!({
                            "category": "stderr",
                            "output": format!("Error: {e}\n"),
                        })),
                    ));
                }
            }
        }

        // Send terminated event
        let seq = self.next_seq();
        responses.push(DapResponse::event(seq, "terminated", None));

        responses
    }

    fn run_program(&mut self, source: String) -> Result<String, String> {
        let mut lexer = Lexer::new(&source);
        let tokens = lexer.tokenize().map_err(|e| e.to_string())?;
        let mut parser = Parser::new(tokens);
        let program = parser.parse().map_err(|e| e.to_string())?;
        let chunk = Compiler::new()
            .compile(&program)
            .map_err(|e| e.to_string())?;

        let mut vm = Vm::new();
        register_vm_stdlib(&mut vm);
        vm.execute(&chunk).map_err(|e| e.to_string())?;

        let output = vm.output().to_string();
        self.vm = Some(vm);
        Ok(output)
    }

    fn handle_continue(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        self.step_mode = StepMode::Continue;
        self.stopped = false;
        let seq = self.next_seq();
        vec![DapResponse::success(
            seq,
            msg.seq,
            "continue",
            Some(json!({ "allThreadsContinued": true })),
        )]
    }

    fn handle_next(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        self.step_mode = StepMode::StepOver;
        let seq = self.next_seq();
        vec![DapResponse::success(seq, msg.seq, "next", None)]
    }

    fn handle_step_in(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        self.step_mode = StepMode::StepIn;
        let seq = self.next_seq();
        vec![DapResponse::success(seq, msg.seq, "stepIn", None)]
    }

    fn handle_step_out(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        self.step_mode = StepMode::StepOut;
        let seq = self.next_seq();
        vec![DapResponse::success(seq, msg.seq, "stepOut", None)]
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
        let frame = StackFrame {
            id: 1,
            name: "pipeline".to_string(),
            line: self.current_line.max(1),
            column: 1,
            source: self.source_path.as_ref().map(|p| Source {
                name: std::path::Path::new(p)
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string()),
                path: Some(p.clone()),
            }),
        };

        let seq = self.next_seq();
        vec![DapResponse::success(
            seq,
            msg.seq,
            "stackTrace",
            Some(json!({
                "stackFrames": [frame],
                "totalFrames": 1,
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

    fn handle_variables(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let vars: Vec<Variable> = self
            .variables
            .iter()
            .map(|(name, val)| Variable {
                name: name.clone(),
                value: val.display(),
                var_type: vm_type_name(val).to_string(),
                variables_reference: 0,
            })
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

        // Try to look up the expression as a variable name
        let result = self
            .variables
            .get(expression)
            .map(|v| v.display())
            .unwrap_or_else(|| format!("<undefined: {expression}>"));

        let seq = self.next_seq();
        vec![DapResponse::success(
            seq,
            msg.seq,
            "evaluate",
            Some(json!({
                "result": result,
                "variablesReference": 0,
            })),
        )]
    }

    fn handle_disconnect(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let seq = self.next_seq();
        vec![DapResponse::success(seq, msg.seq, "disconnect", None)]
    }
}

fn vm_type_name(val: &VmValue) -> &'static str {
    match val {
        VmValue::String(_) => "string",
        VmValue::Int(_) => "int",
        VmValue::Float(_) => "float",
        VmValue::Bool(_) => "bool",
        VmValue::Nil => "nil",
        VmValue::List(_) => "list",
        VmValue::Dict(_) => "dict",
        VmValue::Closure(_) => "closure",
    }
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
            .insert("name".to_string(), VmValue::String("hello".to_string()));

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
    }

    #[test]
    fn test_step_commands() {
        let mut dbg = Debugger::new();

        let r = dbg.handle_message(make_request(1, "next", None));
        assert_eq!(r[0].success, Some(true));
        assert_eq!(dbg.step_mode, StepMode::StepOver);

        let r = dbg.handle_message(make_request(2, "stepIn", None));
        assert_eq!(r[0].success, Some(true));
        assert_eq!(dbg.step_mode, StepMode::StepIn);

        let r = dbg.handle_message(make_request(3, "stepOut", None));
        assert_eq!(r[0].success, Some(true));
        assert_eq!(dbg.step_mode, StepMode::StepOut);

        let r = dbg.handle_message(make_request(4, "continue", None));
        assert_eq!(r[0].success, Some(true));
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
        assert_eq!(frames[0]["line"], 5);
    }
}
