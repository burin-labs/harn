//! Publication round-trip across live sessions, durable journals and saved views.

use crate::agent_sessions::{
    clear_journal, inject_message, install_journal, journal_store, open_or_create_for_test,
    reset_session_store, settle_assistant_publication, transcript,
};
use crate::schema::json_to_vm_value as json_to_vm;
use crate::value::{VmDictExt, VmValue};

#[tokio::test]
async fn admitted_publication_survives_canonical_journal_hydration() {
    reset_session_store();
    let root = tempfile::tempdir().expect("journal root");
    let session_id = "publication-journal-round-trip";
    let mut options = crate::value::DictMap::new();
    options.put_str("root", root.path().to_string_lossy().as_ref());
    let prepared = crate::agent_session_journal::prepare(
        session_id,
        &options,
        "run-first".into(),
        "turn-first".into(),
    )
    .await
    .expect("prepare canonical journal");
    open_or_create_for_test(Some(session_id.into()));
    install_journal(session_id, prepared.state).expect("install journal");
    let mut message = json_to_vm(&serde_json::json!({
        "role":"assistant", "content":"Accepted answer",
    }));
    crate::llm::assistant_publication::defer(
        &mut message,
        &json_to_vm(&serde_json::json!({"_defer_visible":true})),
    );
    inject_message(session_id, message).expect("record actor draft");
    assert_eq!(
        crate::llm::agent_result_projection::last_assistant_text(
            &transcript(session_id).expect("session"),
        ),
        None
    );
    assert_eq!(
        settle_assistant_publication(session_id, true).expect("admit"),
        Some("Accepted answer".into())
    );
    assert_eq!(
        settle_assistant_publication(session_id, true).expect("idempotent settle"),
        None
    );
    crate::agent_session_journal::flush(session_id)
        .await
        .expect("persist admission");
    let store = journal_store(session_id).expect("installed canonical store");
    let run = crate::orchestration::project_run_record_from_session(&store, session_id)
        .await
        .expect("project admitted reply from canonical journal");
    let view = crate::orchestration::build_run_view(&run);
    assert_eq!(view.visible_text.as_deref(), Some("Accepted answer"));
    assert_eq!(view.transcript.message_count, 1, "no public reply copy");
    clear_journal(session_id);
    let hydrated = crate::agent_session_journal::prepare(
        session_id,
        &options,
        "run-second".into(),
        "turn-second".into(),
    )
    .await
    .expect("hydrate canonical journal");
    assert_eq!(hydrated.transcript.messages.len(), 1);
    let message = json_to_vm(&hydrated.transcript.messages[0]);
    assert_eq!(
        crate::llm::agent_result_projection::visible_assistant_text(&message),
        Some("Accepted answer".into())
    );
    assert_eq!(
        message
            .as_dict()
            .and_then(|dict| dict.get("content"))
            .map(VmValue::display),
        Some("Accepted answer".into())
    );
    drop(hydrated);
    reset_session_store();
}

/// The agent loop, not a hand-built draft, decides publication here. A finished
/// run's saved view carries the reply the loop admitted (harn#9507); prose the
/// loop withheld or rejected stays out of every saved view.
#[tokio::test(flavor = "current_thread")]
async fn saved_run_view_carries_the_reply_a_real_agent_loop_admitted() {
    reset_session_store();
    crate::reset_thread_local_state();
    let root = tempfile::tempdir().expect("session root");
    let root_literal =
        serde_json::to_string(root.path().to_str().expect("UTF-8 path")).expect("encode root");
    let source = r###"
import { agent_loop } from "std/agent/loop"
import { agent_options } from "std/agent/options"
import { llm_text, with_llm_script } from "std/testing"

fn run(harness: Harness, session_id: string, replies: list, max_iterations: int) {
  const tools = tool_define(
    tool_registry(),
    "lookup",
    "Read deterministic evidence",
    {
      handler: { _args -> "known evidence" },
      parameters: {},
      returns: {type: "string"},
      annotations: {kind: "read"},
    },
  )
  return with_llm_script(
    harness.llm,
    replies,
    { ->
      return agent_loop(
        harness,
        "Read the evidence, then answer",
        nil,
        agent_options(
          {
            provider: "mock",
            root: ROOT,
            session_id: session_id,
            tools: tools,
            tool_format: "native",
            loop_until_done: true,
            max_iterations: max_iterations,
            max_nudges: 0,
            done_sentinel: "##DONE##",
            final_wrapup: false,
          },
        ),
      )
    },
  )
}

fn main(harness: Harness) {
  const batch = {
    text: "Tool-batch prose before the lookup ran.",
    tool_calls: [{id: "lookup-1", name: "lookup", arguments: {}}],
  }
  const finished = run(
    harness,
    "admitted-reply",
    [batch, llm_text("The evidence is known. ##DONE##")],
    3,
  )
  assert_eq(finished.visible_text, "The evidence is known.")
  const rejected = run(harness, "rejected-reply", [llm_text("Unfinished notes.")], 1)
  assert_eq(rejected.visible_text ?? "", "")
}
"###
    .replace("ROOT", &root_literal);
    let chunk = crate::compile_source(&source).expect("compile agent-loop fixture");
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut vm = crate::Vm::new();
            crate::register_vm_stdlib(&mut vm);
            vm.execute(&chunk).await.expect("run both agent loops");
        })
        .await;

    let mut views = std::collections::BTreeMap::new();
    for session in ["admitted-reply", "rejected-reply"] {
        let store = crate::stdlib::session_store::open_canonical_agent_session(
            &crate::stdlib::session_store::SessionStoreDir::under_root(root.path()),
            session,
            None,
            harn_session_store::SessionType::User,
        )
        .await
        .expect("open persisted session");
        let run = crate::orchestration::project_run_record_from_session(&store, session)
            .await
            .expect("project saved run");
        views.insert(
            session,
            (
                run.status.clone(),
                crate::orchestration::build_run_view(&run),
            ),
        );
    }

    let (status, view) = &views["admitted-reply"];
    assert_eq!(status, "completed");
    assert_eq!(view.visible_text.as_deref(), Some("The evidence is known."));
    let (status, view) = &views["rejected-reply"];
    assert_ne!(status, "completed", "the rejected control must not finish");
    assert_eq!(
        view.visible_text, None,
        "rejected prose must stay unpublished"
    );
    reset_session_store();
    crate::reset_thread_local_state();
}
