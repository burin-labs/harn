//! A rename waiting on a language server that never settles stops when its
//! caller stops it, says why, and writes nothing.
//!
//! The server is a shell script that answers `initialize` and then never
//! reports quiescence, as rust-analyzer does while loading a large workspace.
//! Its bounds are real time because a child process cannot run on a virtual
//! clock; each stop is driven by the rename's own progress heartbeat.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use harn_vm::agent_events::{AgentEvent, AgentEventSink};
use harn_vm::VmValue;
use tempfile::{tempdir, TempDir};

use crate::code_index::state::IndexState;
use crate::code_index::CodeIndexCapability;
use crate::host_call::{CallAddress, HostCallContext, PROGRESS_HEARTBEAT};
use crate::test_support::*;

const NEVER_QUIESCENT: &str = r#"#!/bin/sh
len=0
while IFS= read -r line; do
  line=$(printf '%s' "$line" | tr -d '\r')
  [ -z "$line" ] && break
  case "$line" in Content-Length:*) len=${line#Content-Length: } ;; esac
done
head -c "$len" > /dev/null
body='{"jsonrpc":"2.0","id":1,"result":{"capabilities":{"renameProvider":true}}}'
printf 'Content-Length: %s\r\n\r\n%s' "${#body}" "$body"
exec cat > /dev/null
"#;

const CRATE: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        "[package]\nname = \"small\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    ),
    ("src/lib.rs", "pub mod reading;\npub mod use_it;\n"),
    ("src/reading.rs", "pub struct Reading;\n"),
    (
        "src/use_it.rs",
        "use crate::reading::Reading;\n\npub fn keep(value: Reading) -> Reading {\n    value\n}\n",
    ),
];

struct Fixture {
    _server: TempDir,
    workspace: TempDir,
    capability: CodeIndexCapability,
}

fn fixture() -> Fixture {
    let server = tempdir().unwrap();
    let program = server.path().join("rust-analyzer");
    fs::write(&program, NEVER_QUIESCENT).unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    super::test_override::set("rust-analyzer", &program.to_string_lossy());
    let workspace = tempdir().unwrap();
    for (path, text) in CRATE {
        let path = workspace.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    let capability = CodeIndexCapability::new();
    let (state, _) = IndexState::build_from_root(workspace.path());
    *capability.shared().lock().unwrap() = Some(state);
    Fixture {
        _server: server,
        workspace,
        capability,
    }
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.insert(path.clone(), fs::read(&path).unwrap());
            }
        }
    }
    files
}

fn rename(fixture: &Fixture, extra: &[(&str, VmValue)]) -> VmValue {
    let symbol = dict(&[
        ("name", vm_string("Reading")),
        ("path", vm_string("src/reading.rs")),
        ("kind", vm_string("Type")),
    ]);
    let mut entries = vec![
        ("symbol_ref", symbol),
        ("new_name", vm_string("Measurement")),
        ("scope", vm_string("workspace")),
        ("backend", vm_string("auto")),
    ];
    entries.extend(extra.iter().cloned());
    let request = dict(&entries);
    crate::code_index::rename::run(&fixture.capability.shared(), &[request]).expect("rename runs")
}

/// Records each progress update and forwards the first heartbeat, a report
/// at least one heartbeat into the wait, to the test.
struct Progress {
    phases: Mutex<Vec<(String, u64)>>,
    heartbeat: Mutex<Option<Sender<()>>>,
}

impl AgentEventSink for Progress {
    fn handle_event(&self, event: &AgentEvent) {
        let AgentEvent::ToolCallUpdate {
            data: Some(data), ..
        } = event
        else {
            return;
        };
        let Some(progress) = data.get("progress") else {
            return;
        };
        let phase = progress["phase"].as_str().unwrap_or_default().to_string();
        let elapsed_ms = progress["elapsed_ms"].as_u64().unwrap_or_default();
        self.phases.lock().unwrap().push((phase, elapsed_ms));
        if elapsed_ms >= PROGRESS_HEARTBEAT.as_millis() as u64 {
            if let Some(heartbeat) = self.heartbeat.lock().unwrap().take() {
                let _ = heartbeat.send(());
            }
        }
    }
}

#[test]
fn a_cancel_stops_the_wait_within_a_heartbeat_and_writes_nothing() {
    let _env = crate::process::test_support::declare_inherited();
    let fixture = fixture();
    let before = snapshot(fixture.workspace.path());
    let session = "host-call-cancel";
    let (heartbeat_tx, heartbeat_rx) = channel();
    let progress = Arc::new(Progress {
        phases: Mutex::new(Vec::new()),
        heartbeat: Mutex::new(Some(heartbeat_tx)),
    });
    harn_vm::agent_events::register_sink(session, progress.clone());
    let cancel = Arc::new(AtomicBool::new(false));
    let canceller = std::thread::spawn({
        let cancel = cancel.clone();
        move || {
            heartbeat_rx.recv().expect("the wait reports a heartbeat");
            cancel.store(true, Ordering::SeqCst);
            Instant::now()
        }
    });

    let _interrupt = harn_vm::op_interrupt::install(Some(cancel), None);
    let _call = HostCallContext::new(Some(CallAddress {
        session_id: session.into(),
        tool_call_id: "rename-1".into(),
        tool_name: "rename_symbol".into(),
    }))
    .enter();
    let result = rename(&fixture, &[]);
    let returned = Instant::now();
    let cancelled_at = canceller.join().unwrap();
    harn_vm::agent_events::clear_session_sinks(session);

    let stop = returned.duration_since(cancelled_at);
    eprintln!("cancel to return: {stop:?}");
    assert_eq!(s(field(&result, "result")), "cancelled", "{result:?}");
    let backend = field(&result, "backend");
    assert_eq!(s(field(backend, "server")), "rust-analyzer", "{result:?}");
    assert_eq!(s(field(backend, "server_start")), "cold", "{result:?}");
    assert!(
        stop < PROGRESS_HEARTBEAT,
        "the wait stopped {stop:?} after the cancel"
    );
    let phases = progress.phases.lock().unwrap().clone();
    assert!(
        phases.iter().all(|(phase, _)| phase == "server_start"),
        "{phases:?}"
    );
    assert_eq!(
        snapshot(fixture.workspace.path()),
        before,
        "a stopped rename must leave every file byte-identical"
    );
}

#[test]
fn a_deadline_refuses_with_a_retry_hint_instead_of_falling_back() {
    let _env = crate::process::test_support::declare_inherited();
    let fixture = fixture();
    let before = snapshot(fixture.workspace.path());
    let deadline_ms = 1_500;
    let started = Instant::now();
    let result = rename(&fixture, &[("deadline_ms", VmValue::Int(deadline_ms))]);
    let elapsed = started.elapsed();

    assert_eq!(s(field(&result, "result")), "refused", "{result:?}");
    assert!(
        matches!(field(&result, "retry_after_ms"), VmValue::Int(ms) if *ms >= 1_000),
        "{result:?}"
    );
    assert_eq!(
        s(field(field(&result, "alternative"), "backend")),
        "syntax_tree",
        "{result:?}"
    );
    assert!(
        elapsed.as_millis() < deadline_ms as u128 + PROGRESS_HEARTBEAT.as_millis(),
        "refused after {elapsed:?}"
    );
    assert_eq!(snapshot(fixture.workspace.path()), before);

    // The alternative answers at once and does write.
    let alternative = rename(&fixture, &[("backend", vm_string("syntax_tree"))]);
    assert_eq!(
        s(field(&alternative, "result")),
        "applied",
        "{alternative:?}"
    );
    assert_ne!(snapshot(fixture.workspace.path()), before);
}

/// Runs `on_applying` on the rename's own thread when it reports `applying`,
/// the moment the commit point begins.
struct OnApplying(Box<dyn Fn() + Send + Sync>);

impl AgentEventSink for OnApplying {
    fn handle_event(&self, event: &AgentEvent) {
        if let AgentEvent::ToolCallUpdate {
            data: Some(data), ..
        } = event
        {
            if data["progress"]["phase"] == "applying" {
                (self.0)();
            }
        }
    }
}

fn syntax_rename_at_commit(
    fixture: &Fixture,
    session: &str,
    cancel: Arc<AtomicBool>,
    on_applying: impl Fn() + Send + Sync + 'static,
) -> VmValue {
    harn_vm::agent_events::register_sink(session, Arc::new(OnApplying(Box::new(on_applying))));
    let _interrupt = harn_vm::op_interrupt::install(Some(cancel), None);
    let _call = HostCallContext::new(Some(CallAddress {
        session_id: session.into(),
        tool_call_id: "rename-commit".into(),
        tool_name: "rename_symbol".into(),
    }))
    .enter();
    let result = rename(fixture, &[("backend", vm_string("syntax_tree"))]);
    harn_vm::agent_events::clear_session_sinks(session);
    result
}

#[test]
fn a_cancel_once_applying_began_lets_the_whole_write_finish() {
    let fixture = fixture();
    let cancel = Arc::new(AtomicBool::new(false));
    let result = syntax_rename_at_commit(&fixture, "host-call-applying", cancel.clone(), {
        let cancel = cancel.clone();
        move || cancel.store(true, Ordering::SeqCst)
    });

    assert!(
        cancel.load(Ordering::SeqCst),
        "the commit point reported applying"
    );
    assert_eq!(s(field(&result, "result")), "applied", "{result:?}");
    let read = |path: &str| fs::read_to_string(fixture.workspace.path().join(path)).unwrap();
    assert_eq!(read("src/reading.rs"), "pub struct Measurement;\n");
    assert_eq!(
        read("src/use_it.rs"),
        "use crate::reading::Measurement;\n\npub fn keep(value: Measurement) -> Measurement {\n    value\n}\n"
    );
}

#[test]
fn a_file_changed_by_another_writer_refuses_the_whole_rename() {
    let fixture = fixture();
    let before = snapshot(fixture.workspace.path());
    let other_writer = fixture.workspace.path().join("src/use_it.rs");
    let concurrent = "// written by another tool\n";
    let result = syntax_rename_at_commit(
        &fixture,
        "host-call-concurrent",
        Arc::new(AtomicBool::new(false)),
        {
            let other_writer = other_writer.clone();
            move || {
                let mut text = fs::read_to_string(&other_writer).unwrap();
                text.push_str(concurrent);
                fs::write(&other_writer, text).unwrap();
            }
        },
    );

    assert_eq!(s(field(&result, "result")), "refused", "{result:?}");
    assert!(
        s(field(&result, "details")).contains("src/use_it.rs"),
        "{result:?}"
    );
    let mut expected = before;
    let original = expected.get_mut(&other_writer).unwrap();
    original.extend_from_slice(concurrent.as_bytes());
    assert_eq!(
        snapshot(fixture.workspace.path()),
        expected,
        "only the other writer's change may land"
    );
}
