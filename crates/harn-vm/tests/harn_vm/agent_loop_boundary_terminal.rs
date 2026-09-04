//! What a throw at the agent-loop boundary seals as the run's terminal.
//!
//! A throw that reaches the loop boundary is not automatically a failure. When
//! a stop is accepted, the session is torn down under whatever call was in
//! flight, and that call throws because its session vanished rather than
//! because anything went wrong. The boundary stamped every such throw
//! `failed`/`error`, so a run that was told to stop and a run that broke were
//! indistinguishable to everything downstream (harn#7909).
//!
//! The instrument is the persisted `agent_run_terminal` event, not the
//! transcript sidecar: no sidecar is written on this throw path, so a probe
//! that reads one measures nothing and reports it as an absence.
//!
//! Both directions are asserted, because the cancelled arm alone would pass
//! for an implementation that called every boundary throw a cancellation.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use harn_vm::bridge::HostBridge;
use harn_vm::value::VmError;

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);

fn fresh_session_id(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{}",
        std::process::id(),
        SESSION_COUNTER.fetch_add(1, Ordering::Relaxed),
    )
}

/// The pipeline: run one agent loop whose completion check throws `thrown`.
fn boundary_throw_pipeline(session_id: &str, store_root: &str, thrown: &str) -> String {
    format!(
        r###"
import {{ llm_text, with_llm_script }} from "std/testing"

pipeline default(harness: Harness) {{
  const outcome = try {{
    with_llm_script(
      harness.llm,
      [llm_text("working ##DONE##")],
      {{ ->
        return agent_loop(
          harness,
          "Do it.",
          nil,
          {{
            provider: "mock",
            model: "boundary-actor",
            session_id: "{session_id}",
            root: "{store_root}",
            max_iterations: 2,
            loop_until_done: true,
            done_sentinel: "##DONE##",
            verify_completion: {{ info -> throw {thrown} }},
          }},
        )
      }},
    )
  }}
  // The loop rethrows either way; the terminal it sealed is the claim.
  harness.stdio.log(to_string(is_err(outcome)))
}}
"###
    )
}

/// Run one pipeline against a fresh session store and return its stdout.
///
/// The VM must not run on the libtest thread's stack: it is large enough
/// only because the CI lanes export `RUST_MIN_STACK`, and an overflow aborts
/// the whole binary instead of failing this case. `on_vm_stack` enters the
/// VM on a thread sized for the contract regardless of the ambient
/// environment.
fn run_pipeline(source: &str, store: &tempfile::TempDir) -> Result<String, String> {
    let source = source.to_string();
    let _ = store;
    harn_vm::on_vm_stack(move || run_pipeline_here(&source))
}

fn run_pipeline_here(source: &str) -> Result<String, String> {
    harn_vm::reset_thread_local_state();
    let source = format!("import {{ agent_loop }} from \"std/agent/loop\"\n{source}");
    let chunk = harn_vm::compile_source(&source)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let bridge = Arc::new(HostBridge::from_parts(
                    Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(Mutex::new(())),
                    1,
                ));
                harn_vm::llm::install_current_host_bridge(bridge.clone());
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                let result = vm
                    .execute(&chunk)
                    .await
                    .map_err(|e: VmError| format!("{e:?}"));
                harn_vm::llm::clear_current_host_bridge();
                result?;
                Ok(vm.output().to_string())
            })
            .await
    })
}

/// Every `custom_kind` the session recorded, in order, plus the payload of the
/// single `agent_run_terminal` event.
struct SealedTerminal {
    kinds: Vec<String>,
    terminal: serde_json::Value,
}

fn read_sealed_terminal(store: &std::path::Path, session_id: &str) -> SealedTerminal {
    let db = store.join(".harn").join("session-store.sqlite");
    assert!(
        db.is_file(),
        "no session store at {}: the run wrote nothing, so any verdict here would be measuring \
         nothing rather than measuring a terminal",
        db.display(),
    );
    let conn = rusqlite::Connection::open(&db).expect("open session store");
    let mut statement = conn
        .prepare(
            "select coalesce(custom_kind, kind), payload_json from session_events \
             where session_id = ?1 order by event_id",
        )
        .expect("prepare");
    let rows: Vec<(String, String)> = statement
        .query_map([session_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query")
        .map(|row| row.expect("row"))
        .collect();
    assert!(
        !rows.is_empty(),
        "session {session_id} recorded no events; a zero here is not a measurement",
    );

    let kinds: Vec<String> = rows.iter().map(|(kind, _)| kind.clone()).collect();
    let terminal_payload = rows
        .iter()
        .find(|(kind, _)| kind == "agent_run_terminal")
        .map(|(_, payload)| payload.clone())
        .unwrap_or_else(|| panic!("no agent_run_terminal event among {kinds:?}"));
    let parsed: serde_json::Value =
        serde_json::from_str(&terminal_payload).expect("terminal payload is json");
    let terminal = parsed
        .pointer("/transcript_event/metadata")
        .cloned()
        .expect("terminal metadata");
    SealedTerminal { kinds, terminal }
}

fn seal_boundary_throw(thrown: &str, prefix: &str) -> SealedTerminal {
    let store = tempfile::tempdir().expect("temp store");
    let session_id = fresh_session_id(prefix);
    let root = store.path().to_string_lossy().replace('\\', "\\\\");
    let source = boundary_throw_pipeline(&session_id, &root, thrown);
    let output = run_pipeline(&source, &store).expect("pipeline ran");
    assert!(
        output.contains("true"),
        "the loop must still rethrow; got {output:?}",
    );
    read_sealed_terminal(store.path(), &session_id)
}

#[test]
fn an_accepted_stop_seals_a_cancelled_terminal_without_an_error() {
    let sealed = seal_boundary_throw(
        r#"{category: "cancelled", message: "session cancelled"}"#,
        "boundary-cancelled",
    );

    assert_eq!(sealed.terminal["final_status"], "cancelled");
    assert_eq!(sealed.terminal["stop_reason"], "cancelled");
    assert_eq!(sealed.terminal["terminal"]["kind"], "user_cancelled");
    assert_eq!(sealed.terminal["terminal"]["owner"], "user");
    // No error key. A cancellation is not an error, and carrying one is what
    // fired the session-error hook.
    assert!(
        sealed.terminal["error"].is_null(),
        "a cancelled terminal must carry no error, got {}",
        sealed.terminal["error"],
    );
    // The other half of the failed lifecycle: the session-error event must not
    // have been emitted at all.
    assert!(
        !sealed
            .kinds
            .iter()
            .any(|kind| kind == "agent_loop_terminal_error"),
        "a cancelled run must not emit the session-error event; kinds were {:?}",
        sealed.kinds,
    );
}

#[test]
fn any_other_boundary_throw_still_seals_a_failure() {
    // The control. Without it the assertions above would also pass for an
    // implementation that called every boundary throw a cancellation.
    let sealed = seal_boundary_throw(
        r#"{category: "generic", message: "real bug"}"#,
        "boundary-generic",
    );

    assert_eq!(sealed.terminal["final_status"], "failed");
    assert_eq!(sealed.terminal["stop_reason"], "error");
    assert_eq!(sealed.terminal["terminal"]["kind"], "runtime_error");
    assert_eq!(sealed.terminal["terminal"]["owner"], "harness");
    assert_eq!(sealed.terminal["error"]["message"], "real bug");
    assert!(
        sealed
            .kinds
            .iter()
            .any(|kind| kind == "agent_loop_terminal_error"),
        "a genuine failure must still emit the session-error event; kinds were {:?}",
        sealed.kinds,
    );
}
