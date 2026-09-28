//! A background command handle's lifetime, end to end: it belongs to the
//! session that started it, survives the agent-loop run (one turn) that
//! started it, and ends when the session closes.

#![cfg(unix)]

use harn_vm::VmValue;

use super::process_tools_e2e::{call, dict, require_dict, require_str, vlist_str, vstr};

/// A background handle belongs to its session, not to the agent loop that
/// started it. A host opens a session once and runs one `agent_loop` per turn,
/// so a handle cancelled when that run finishes is gone before the next turn
/// can poll it, and the model starts duplicates. Closing the session still
/// ends the handle.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn background_handle_outlives_its_turn_and_ends_with_its_session() {
    let session_id = format!("host-session-handle-{}", std::process::id());
    let turn = run_turn_after_backgrounding(&session_id);
    harn_hostlib::tools::long_running::cancel_session_handles(&session_id);
    assert_eq!(
        turn.after_loop,
        vec![turn.handle_id],
        "the handle must still be listed after the turn's agent loop returns"
    );
    assert!(
        turn.after_close.is_empty(),
        "closing the session must end its handles: {:?}",
        turn.after_close
    );
}

struct BackgroundedTurn {
    handle_id: String,
    after_loop: Vec<String>,
    after_close: Vec<String>,
}

/// Background `sleep 300` under a host-opened `session_id`, run one
/// `agent_loop` turn over the same session, then list the session's handles
/// after the loop returns and again after the session is closed.
fn run_turn_after_backgrounding(session_id: &str) -> BackgroundedTurn {
    fn session_handles(session_id: &str) -> Vec<String> {
        let mut request = dict();
        request.insert("session_id".into(), vstr(session_id));
        let listed =
            require_dict(call("hostlib_tools_list_handles", request).expect("list handles"));
        match listed.get("handles") {
            Some(VmValue::List(handles)) => handles
                .iter()
                .map(|handle| match handle {
                    VmValue::Dict(row) => require_str(row, "handle_id"),
                    other => panic!("expected a handle row, got {other:?}"),
                })
                .collect(),
            other => panic!("expected a handle list, got {other:?}"),
        }
    }
    let source = format!(
        r#"import {{ agent_loop }} from "std/agent/loop"

fn main(harness: Harness) {{
  harness.llm.mock_clear()
  harness.llm.mock_enqueue({{text: "done for this turn"}})
  agent_loop(harness, "next turn", nil, {{
    provider: "mock",
    tool_format: "native",
    session_id: "{session_id}",
  }})
}}
"#
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build script runtime");
    runtime.block_on(async {
        tokio::task::LocalSet::new()
            .run_until(async {
                // The host opens the session once, as an ACP server does
                // before a session's first prompt.
                harn_vm::agent_sessions::open_or_create(Some(session_id.to_string()))
                    .expect("host opens the session");
                let handle_id = {
                    let _session =
                        harn_vm::agent_sessions::enter_current_session(session_id.to_string());
                    let mut request = dict();
                    request.insert("argv".into(), vlist_str(&["sleep", "300"]));
                    request.insert("background".into(), VmValue::Bool(true));
                    let started = require_dict(
                        call("hostlib_tools_run_command", request).expect("background sleep"),
                    );
                    require_str(&started, "handle_id")
                };
                assert_eq!(
                    session_handles(session_id),
                    vec![handle_id.clone()],
                    "handle registered"
                );
                let tokens = harn_lexer::Lexer::new(&source)
                    .tokenize()
                    .expect("tokenize");
                let program = harn_parser::Parser::new(tokens).parse().expect("parse");
                let chunk = harn_vm::Compiler::new().compile(&program).expect("compile");
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                let _ = harn_hostlib::install_default(&mut vm);
                vm.set_harness(harn_vm::Harness::real());
                vm.execute(&chunk).await.expect("execute");
                // A cancelled entry leaves the list once its waiter drains
                // the exit, so give an ending handle a bounded moment.
                let settled = |grace: std::time::Duration| {
                    let deadline = std::time::Instant::now() + grace;
                    loop {
                        let handles = session_handles(session_id);
                        if handles.is_empty() || std::time::Instant::now() >= deadline {
                            return handles;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                };
                // A kill at the loop's end would empty the list well inside
                // this window; a handle that stays listed for all of it
                // survived the turn.
                let after_loop = settled(std::time::Duration::from_secs(2));
                harn_vm::agent_sessions::close(session_id);
                BackgroundedTurn {
                    handle_id,
                    after_loop,
                    after_close: settled(std::time::Duration::from_secs(5)),
                }
            })
            .await
    })
}
