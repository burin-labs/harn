//! A background command handle's lifetime, end to end: it belongs to the
//! session that started it, survives the agent-loop run (one turn) that
//! started it, and ends when the session closes.

#![cfg(unix)]

use harn_hostlib::tools::long_running::register_completion_notifier;
use harn_vm::VmValue;

use super::process_tools_e2e::{call, dict, require_dict, require_str, vlist_str, vstr};

/// A background handle belongs to its session, not to the agent loop that
/// started it. A host opens a session once and runs one `agent_loop` per turn,
/// so a handle cancelled when that run finishes is gone before the next turn
/// can poll it, and the model starts duplicates. Closing the session still
/// ends the handle.
///
/// Both verdicts are decisions the handle store made, not timings. A kill at
/// the loop's end marks the handle's cancellation as requested before the loop
/// returns, so a later explicit cancel reports `cancelled: false`; a handle the
/// turn left alone reports `true`. Closing the session is observed through the
/// handle's completion notifier.
#[test]
fn background_handle_outlives_its_turn_and_ends_with_its_session() {
    let session_id = format!("host-session-handle-{}", std::process::id());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build script runtime");
    runtime.block_on(async {
        tokio::task::LocalSet::new()
            .run_until(async {
                // The host opens the session once, as an ACP server does
                // before a session's first prompt.
                harn_vm::agent_sessions::open_or_create(Some(session_id.clone()))
                    .expect("host opens the session");
                let probed = background_sleep(&session_id);
                let kept = background_sleep(&session_id);
                run_one_turn(&session_id).await;

                let probed_done = register_completion_notifier(&probed)
                    .expect("the turn must leave the probed handle registered");
                let mut request = dict();
                request.insert("handle_id".into(), vstr(&probed));
                let cancel = require_dict(
                    call("hostlib_tools_cancel_handle", request).expect("cancel probed handle"),
                );
                assert!(
                    matches!(cancel.get("cancelled"), Some(VmValue::Bool(true))),
                    "the turn's loop must not have cancelled the handle: {cancel:?}"
                );
                probed_done.recv().expect("probed handle completes");

                let kept_done = register_completion_notifier(&kept)
                    .expect("the turn must leave the kept handle registered");
                harn_vm::agent_sessions::close(&session_id);
                kept_done
                    .recv()
                    .expect("closing the session ends its handle");
                assert!(
                    session_handles(&session_id).is_empty(),
                    "closing the session must end its handles"
                );
            })
            .await;
    });
}

/// Background `sleep 300` as `session_id`, returning its handle id.
fn background_sleep(session_id: &str) -> String {
    let _session = harn_vm::agent_sessions::enter_current_session(session_id.to_string());
    let mut request = dict();
    request.insert("argv".into(), vlist_str(&["sleep", "300"]));
    request.insert("background".into(), VmValue::Bool(true));
    let started = require_dict(call("hostlib_tools_run_command", request).expect("background"));
    require_str(&started, "handle_id")
}

/// Run one `agent_loop` turn with a mock model over `session_id`.
async fn run_one_turn(session_id: &str) {
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
}

fn session_handles(session_id: &str) -> Vec<String> {
    let mut request = dict();
    request.insert("session_id".into(), vstr(session_id));
    let listed = require_dict(call("hostlib_tools_list_handles", request).expect("list handles"));
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
