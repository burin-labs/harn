use std::sync::{Arc, Mutex};

use harn_runtime::register_vm_stdlib;
use harn_vm::run_events::{self, RunEvent, RunEventKind, RunEventSink};

#[derive(Default)]
struct CapturingSink {
    events: Mutex<Vec<RunEvent>>,
}

impl RunEventSink for CapturingSink {
    fn emit(&self, event: RunEvent) {
        self.events.lock().unwrap().push(event);
    }
}

#[derive(Default)]
struct DiagnosticSink(CapturingSink);

impl RunEventSink for DiagnosticSink {
    fn enabled(&self, kind: RunEventKind) -> bool {
        kind == RunEventKind::Stderr
    }

    fn emit(&self, event: RunEvent) {
        self.0.emit(event);
    }
}

async fn execute(sink: Arc<dyn RunEventSink>) -> String {
    let chunk = harn_vm::compile_source(
        r#"
fn main(harness: Harness) {
  harness.stdio.println("ordinary output")
  harness.stdio.eprintln("observed diagnostic")
}
"#,
    )
    .unwrap();
    harn_clock::test_support::with_local_tasks(run_events::scope(sink, async {
        let mut vm = harn_vm::new_vm();
        register_vm_stdlib(&mut vm);
        vm.execute(&chunk).await.unwrap();
        vm.output().to_string()
    }))
    .await
}

#[tokio::test(flavor = "current_thread")]
async fn selected_diagnostics_preserve_ordinary_vm_stdout() {
    let sink = Arc::new(DiagnosticSink::default());
    assert_eq!(execute(sink.clone()).await, "ordinary output\n");
    let events = sink.0.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert!(
        matches!(&events[0], RunEvent::Stderr { payload } if payload == "observed diagnostic\n")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn existing_sinks_still_capture_both_vm_output_streams() {
    let sink = Arc::new(CapturingSink::default());
    assert_eq!(execute(sink.clone()).await, "");
    let events = sink.events.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert!(matches!(&events[0], RunEvent::Stdout { payload } if payload == "ordinary output\n"));
    assert!(
        matches!(&events[1], RunEvent::Stderr { payload } if payload == "observed diagnostic\n")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn selection_is_inherited_and_restored_without_forwarding_other_kinds() {
    let outer = Arc::new(CapturingSink::default());
    let selected = Arc::new(DiagnosticSink::default());
    run_events::scope(outer.clone(), async {
        assert!(run_events::enabled(RunEventKind::Stdout));
        run_events::scope(selected.clone(), async {
            assert!(!run_events::enabled(RunEventKind::Stdout));
            harn_vm::orchestration::scope_inline_subtask(async {
                assert!(run_events::enabled(RunEventKind::Stderr));
                run_events::emit(RunEvent::Hook {
                    name: "unselected".into(),
                    phase: "before".into(),
                    payload: serde_json::json!({"value": "not forwarded"}),
                });
                run_events::emit(RunEvent::Stderr {
                    payload: "selected child\n".into(),
                });
            })
            .await;
        })
        .await;
        assert!(run_events::enabled(RunEventKind::Stdout));
        run_events::emit(RunEvent::Stdout {
            payload: "restored outer\n".into(),
        });
    })
    .await;
    assert!(!run_events::enabled(RunEventKind::Stderr));
    let events = selected.0.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], RunEvent::Stderr { payload } if payload == "selected child\n"));
    let events = outer.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], RunEvent::Stdout { payload } if payload == "restored outer\n"));
}
