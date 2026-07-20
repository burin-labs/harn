//! End-to-end smoke test that drives the real `harn-dap` binary over stdio,
//! speaking the `Content-Length`-framed Debug Adapter Protocol exactly as an
//! editor would. This is deliberately a black-box test against the spawned
//! process (not the in-crate `Debugger` API) so it exercises the actual
//! transport: the stdin reader thread, frame parsing, the interleaved
//! step/drain loop, and stdout framing/flushing.
//!
//! It proves the debugger works — not just compiles — by hitting a breakpoint,
//! reading back a live local variable's value, then running to completion. If
//! any of those regress, this test fails instead of the debugger silently
//! rotting behind the (unpublished) VS Code extension.

use std::io::BufReader;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

fn harn_dap_bin() -> String {
    std::env::var("CARGO_BIN_EXE_harn-dap")
        .or_else(|_| std::env::var("NEXTEST_BIN_EXE_harn-dap"))
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_harn-dap").to_string())
}

/// Per-message read bound. Generous — this catches a *wedged* adapter (the
/// failure mode this smoke test exists to detect), not slow-but-working
/// responses. A dedicated reader thread feeds an mpsc channel so we block on
/// `recv_timeout` rather than polling a wall clock (which the flaky-test lint
/// forbids, and rightly so).
const RECV_BUDGET: Duration = Duration::from_secs(30);

/// A DAP client speaking to a spawned `harn-dap` over its stdio pipes.
struct DapClient {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    seq: i64,
}

/// Read exactly one `Content-Length`-framed DAP message from `stdout`.
/// Returns `None` at EOF or on a malformed frame so the reader thread can
/// exit cleanly (closing the channel).
fn read_message(stdout: &mut BufReader<ChildStdout>) -> Option<Value> {
    let body = harn_dap::framing::read_frame(stdout).ok()??;
    serde_json::from_slice(&body).ok()
}

impl DapClient {
    fn spawn() -> Self {
        let mut child = Command::new(harn_dap_bin())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn harn-dap binary");
        let stdin = child.stdin.take().expect("child stdin");
        let mut stdout = BufReader::new(child.stdout.take().expect("child stdout"));
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            while let Some(msg) = read_message(&mut stdout) {
                if tx.send(msg).is_err() {
                    break; // client dropped
                }
            }
        });
        DapClient {
            child,
            stdin,
            rx,
            seq: 0,
        }
    }

    fn send(&mut self, command: &str, arguments: Value) {
        self.seq += 1;
        let mut msg = json!({ "seq": self.seq, "type": "request", "command": command });
        if !arguments.is_null() {
            msg["arguments"] = arguments;
        }
        let body = serde_json::to_vec(&msg).unwrap();
        harn_dap::framing::write_frame(&mut self.stdin, &body).unwrap();
    }

    /// Block for messages until `pred` matches, returning that message. Fails
    /// the test (rather than hanging forever) if no message arrives within the
    /// budget or the adapter closes its stdout first — a wedged handshake is
    /// exactly the failure mode this smoke test must catch.
    fn read_until(&mut self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        loop {
            match self.rx.recv_timeout(RECV_BUDGET) {
                Ok(msg) if pred(&msg) => return msg,
                Ok(_) => continue,
                Err(RecvTimeoutError::Timeout) => {
                    panic!("timed out waiting for {what}; adapter produced no matching message")
                }
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("adapter closed stdout before producing {what}")
                }
            }
        }
    }
}

impl Drop for DapClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn is_event(msg: &Value, name: &str) -> bool {
    msg["type"] == "event" && msg["event"] == name
}

fn is_response(msg: &Value, command: &str) -> bool {
    msg["type"] == "response" && msg["command"] == command
}

#[test]
fn dap_stdio_hits_breakpoint_and_reads_a_local_variable() {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("smoke.harn");
    // Line 2 binds `x = 42`; line 4 (`log(y)`) is where we break — by then
    // both `x` and `y` are in scope.
    std::fs::write(
        &program,
        "pipeline test(task) {\n  const x = 42\n  const y = x + 1\n  log(y)\n}\n",
    )
    .unwrap();
    let program_path = program.to_string_lossy().to_string();
    let break_line = 4;

    let mut dap = DapClient::spawn();

    // initialize -> response + `initialized` event.
    dap.send(
        "initialize",
        json!({ "adapterID": "harn", "linesStartAt1": true, "columnsStartAt1": true }),
    );
    let init_resp = dap.read_until("initialize response", |m| is_response(m, "initialize"));
    assert_eq!(init_resp["success"], json!(true), "initialize must succeed");
    dap.read_until("initialized event", |m| is_event(m, "initialized"));

    // setBreakpoints before launch so the compiled program registers them.
    dap.send(
        "setBreakpoints",
        json!({
            "source": { "path": program_path },
            "breakpoints": [ { "line": break_line } ],
        }),
    );
    let bp_resp = dap.read_until("setBreakpoints response", |m| {
        is_response(m, "setBreakpoints")
    });
    let verified = &bp_resp["body"]["breakpoints"][0];
    assert_eq!(verified["verified"], json!(true), "breakpoint must verify");
    assert_eq!(verified["line"], json!(break_line));

    // launch + configurationDone drive the VM into its run loop.
    dap.send(
        "launch",
        json!({ "program": program_path, "stopOnEntry": false }),
    );
    dap.read_until("launch response", |m| is_response(m, "launch"));
    dap.send("configurationDone", Value::Null);

    // The interleaved step loop must stop at our breakpoint on its own —
    // no further client input needed.
    let stopped = dap.read_until("stopped event", |m| is_event(m, "stopped"));
    assert_eq!(
        stopped["body"]["reason"],
        json!("breakpoint"),
        "must stop for a breakpoint, got: {stopped}"
    );

    // stackTrace: at least one frame, positioned at (or just before) the
    // breakpoint line. The VM reports the current instruction-pointer line,
    // which sits on the statement about to execute — so a breakpoint on the
    // `log(y)` line surfaces the frame on that line or the one above it. We
    // assert a tolerant band rather than an exact line so a benign IP/line
    // rounding change doesn't make this smoke test brittle.
    dap.send("stackTrace", json!({ "threadId": 1 }));
    let stack = dap.read_until("stackTrace response", |m| is_response(m, "stackTrace"));
    let frames = stack["body"]["stackFrames"]
        .as_array()
        .expect("stackFrames");
    assert!(!frames.is_empty(), "expected at least one stack frame");
    let top_line = frames[0]["line"].as_i64().expect("frame line");
    assert!(
        (break_line - 1..=break_line).contains(&top_line),
        "top frame line {top_line} should be at/just above the breakpoint line {break_line}"
    );
    let frame_id = frames[0]["id"].as_i64().expect("frame id");

    // scopes: a Locals scope backed by variablesReference 1.
    dap.send("scopes", json!({ "frameId": frame_id }));
    let scopes = dap.read_until("scopes response", |m| is_response(m, "scopes"));
    let locals = scopes["body"]["scopes"]
        .as_array()
        .expect("scopes")
        .iter()
        .find(|s| s["name"] == "Locals")
        .expect("a Locals scope");
    let vars_ref = locals["variablesReference"]
        .as_i64()
        .expect("variablesReference");

    // variables: the live locals map must carry `x == 42` — the behavioral
    // proof that the debugger observed real VM state at the stop.
    dap.send("variables", json!({ "variablesReference": vars_ref }));
    let vars = dap.read_until("variables response", |m| is_response(m, "variables"));
    let list = vars["body"]["variables"].as_array().expect("variables");
    let x = list
        .iter()
        .find(|v| v["name"] == "x")
        .unwrap_or_else(|| panic!("local `x` missing from {list:?}"));
    assert_eq!(
        x["value"],
        json!("42"),
        "local `x` should read 42 at the breakpoint"
    );

    // continue -> the program runs to completion and terminates.
    dap.send("continue", json!({ "threadId": 1 }));
    dap.read_until("terminated event", |m| is_event(m, "terminated"));
}
