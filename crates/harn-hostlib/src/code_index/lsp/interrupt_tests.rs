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
# fd 3 keeps the reply pipe open, so the client never reads an exit.
exec cat 3>&1 > /dev/null
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
    fixture_with(NEVER_QUIESCENT, "")
}

/// The crate served by `server_script`, with `padding` appended to the
/// declaration's file.
fn fixture_with(server_script: &str, padding: &str) -> Fixture {
    let server = tempdir().unwrap();
    let program = server.path().join("rust-analyzer");
    fs::write(&program, server_script).unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    super::test_override::set("rust-analyzer", &program.to_string_lossy());
    let workspace = tempdir().unwrap();
    for (path, text) in CRATE {
        let path = workspace.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let padding = if path.ends_with("src/reading.rs") {
            padding
        } else {
            ""
        };
        fs::write(path, format!("{text}{padding}")).unwrap();
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
    let canceller = harn_parser::runtime_stack::spawn({
        let cancel = cancel.clone();
        move || {
            harn_clock::test_support::recv_within("the wait's heartbeat", &heartbeat_rx);
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

#[test]
fn another_writer_waits_until_the_commit_has_written_every_file() {
    let fixture = fixture();
    let mut tools = crate::registry::BuiltinRegistry::new();
    crate::registry::HostlibCapability::register_builtins(
        &crate::tools::ToolsCapability,
        &mut tools,
    );
    let write_file = tools
        .iter()
        .find(|builtin| builtin.name == "hostlib_tools_write_file")
        .expect("write_file is registered")
        .handler
        .clone();
    let other_path = fixture.workspace.path().join("src/use_it.rs");
    let other_text = "// written by another tool\n";
    let wrote_during_commit = Arc::new(AtomicBool::new(false));
    let writer = Arc::new(Mutex::new(None));

    let result = syntax_rename_at_commit(
        &fixture,
        "host-call-lease",
        Arc::new(AtomicBool::new(false)),
        {
            let wrote_during_commit = wrote_during_commit.clone();
            let writer = writer.clone();
            move || {
                let done = Arc::new(AtomicBool::new(false));
                let request = dict(&[
                    ("path", vm_string(&other_path.to_string_lossy())),
                    ("content", vm_string(other_text)),
                ]);
                let thread = harn_parser::runtime_stack::spawn({
                    let done = done.clone();
                    let write_file = write_file.clone();
                    move || {
                        write_file(&[request]).expect("the other write lands");
                        done.store(true, Ordering::SeqCst);
                    }
                });
                std::thread::sleep(std::time::Duration::from_millis(200));
                wrote_during_commit.store(done.load(Ordering::SeqCst), Ordering::SeqCst);
                *writer.lock().unwrap() = Some(thread);
            }
        },
    );
    writer
        .lock()
        .unwrap()
        .take()
        .expect("the commit point reported applying")
        .join()
        .unwrap();

    assert!(
        !wrote_during_commit.load(Ordering::SeqCst),
        "another builtin wrote while the rename was committing"
    );
    assert_eq!(s(field(&result, "result")), "applied", "{result:?}");
    let read = |path: &str| fs::read_to_string(fixture.workspace.path().join(path)).unwrap();
    assert_eq!(read("src/reading.rs"), "pub struct Measurement;\n");
    assert_eq!(
        read("src/use_it.rs"),
        other_text,
        "the waiting write lands after"
    );
}

/// A gopls stand-in that loads the project once, then renames every whole
/// word matching the one at the position across its open documents.
const RENAMING_SERVER: &str = r#"#!/usr/bin/env python3
import json, re, sys
inp, out = sys.stdin.buffer, sys.stdout.buffer
docs, loaded = {}, False
def send(msg):
    body = json.dumps(msg).encode()
    out.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    out.flush()
def read():
    length = None
    while True:
        line = inp.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        key, _, value = line.partition(b":")
        if key.lower() == b"content-length":
            length = int(value)
    return json.loads(inp.read(length))
while True:
    msg = read()
    if msg is None or msg.get("method") == "exit":
        break
    method, mid, params = msg.get("method"), msg.get("id"), msg.get("params") or {}
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": mid, "result": {"capabilities": {"renameProvider": True}}})
    elif method == "textDocument/didOpen":
        docs[params["textDocument"]["uri"]] = params["textDocument"]["text"]
        if not loaded:
            loaded = True
            for kind in ("begin", "end"):
                send({"jsonrpc": "2.0", "method": "$/progress", "params": {"token": "load", "value": {"kind": kind, "title": "load"}}})
    elif method == "textDocument/didChange":
        docs[params["textDocument"]["uri"]] = params["contentChanges"][-1]["text"]
    elif method == "textDocument/rename":
        pos = params["position"]
        line = docs[params["textDocument"]["uri"]].split("\n")[pos["line"]]
        word = next(w.group() for w in re.finditer(r"\w+", line) if w.start() <= pos["character"] <= w.end())
        changes = {}
        for uri, text in docs.items():
            edits = [
                {"range": {"start": {"line": i, "character": w.start()}, "end": {"line": i, "character": w.end()}}, "newText": params["newName"]}
                for i, row in enumerate(text.split("\n"))
                for w in re.finditer(r"\b%s\b" % re.escape(word), row)
            ]
            if edits:
                changes[uri] = edits
        send({"jsonrpc": "2.0", "id": mid, "result": {"changes": changes}})
    elif mid is not None:
        send({"jsonrpc": "2.0", "id": mid, "result": None})
"#;

#[test]
fn progress_phases_arrive_in_order() {
    let _env = crate::process::test_support::declare_inherited();
    let server = tempdir().unwrap();
    let program = server.path().join("gopls");
    fs::write(&program, RENAMING_SERVER).unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    super::test_override::set("gopls", &program.to_string_lossy());
    let workspace = tempdir().unwrap();
    fs::write(workspace.path().join("go.mod"), "module small\n\ngo 1.21\n").unwrap();
    let source = "package small\n\ntype Reading struct{}\n\nfunc Keep(value Reading) Reading {\n\treturn value\n}\n";
    fs::write(workspace.path().join("reading.go"), source).unwrap();
    let capability = CodeIndexCapability::new();
    let (state, _) = IndexState::build_from_root(workspace.path());
    *capability.shared().lock().unwrap() = Some(state);

    let session = "host-call-phases";
    let progress = Arc::new(Progress {
        phases: Mutex::new(Vec::new()),
        heartbeat: Mutex::new(None),
    });
    harn_vm::agent_events::register_sink(session, progress.clone());
    let _interrupt = harn_vm::op_interrupt::install(Some(Arc::new(AtomicBool::new(false))), None);
    let _call = HostCallContext::new(Some(CallAddress {
        session_id: session.into(),
        tool_call_id: "rename-phases".into(),
        tool_name: "rename_symbol".into(),
    }))
    .enter();
    let request = dict(&[
        (
            "symbol_ref",
            dict(&[
                ("name", vm_string("Reading")),
                ("path", vm_string("reading.go")),
                ("kind", vm_string("Type")),
            ]),
        ),
        ("new_name", vm_string("Measurement")),
        ("scope", vm_string("workspace")),
        ("backend", vm_string("language_server")),
    ]);
    let result =
        crate::code_index::rename::run(&capability.shared(), &[request]).expect("rename runs");
    harn_vm::agent_events::clear_session_sinks(session);

    assert_eq!(s(field(&result, "result")), "applied", "{result:?}");
    assert_eq!(
        fs::read_to_string(workspace.path().join("reading.go")).unwrap(),
        source.replace("Reading", "Measurement")
    );
    let mut phases: Vec<String> = progress
        .phases
        .lock()
        .unwrap()
        .iter()
        .map(|(phase, _)| phase.clone())
        .collect();
    phases.dedup();
    assert_eq!(
        phases,
        ["server_start", "indexing", "resolving", "applying"],
        "{result:?}"
    );
}

/// Answers `initialize`, reports quiescence, then never reads stdin again, so
/// a large document fills the pipe to it.
const STOPS_READING: &str = r#"#!/bin/sh
len=0
while IFS= read -r line; do
  line=$(printf '%s' "$line" | tr -d '\r')
  [ -z "$line" ] && break
  case "$line" in Content-Length:*) len=${line#Content-Length: } ;; esac
done
head -c "$len" > /dev/null
body='{"jsonrpc":"2.0","id":1,"result":{"capabilities":{"renameProvider":true}}}'
printf 'Content-Length: %s\r\n\r\n%s' "${#body}" "$body"
ready='{"jsonrpc":"2.0","method":"experimental/serverStatus","params":{"health":"ok","quiescent":true}}'
printf 'Content-Length: %s\r\n\r\n%s' "${#ready}" "$ready"
exec sleep 120
"#;

#[test]
fn a_server_that_stops_reading_cannot_hold_the_rename_past_its_deadline() {
    let _env = crate::process::test_support::declare_inherited();
    // Far larger than a pipe buffer, so opening the file fills the pipe.
    let padding = "// padding line that the language server never reads\n".repeat(8_192);
    let fixture = fixture_with(STOPS_READING, &padding);
    let before = snapshot(fixture.workspace.path());
    let result = rename(
        &fixture,
        &[
            ("backend", vm_string("language_server")),
            ("deadline_ms", VmValue::Int(1_500)),
        ],
    );

    assert_eq!(s(field(&result, "result")), "refused", "{result:?}");
    assert_eq!(snapshot(fixture.workspace.path()), before);
}

#[test]
fn a_cancel_while_waiting_for_another_writer_writes_nothing() {
    let fixture = fixture();
    let before = snapshot(fixture.workspace.path());
    let cancel = Arc::new(AtomicBool::new(false));
    let held = crate::host_call::workspace_writes();
    let renamer = harn_parser::runtime_stack::spawn({
        let shared = fixture.capability.shared();
        let cancel = cancel.clone();
        move || {
            let _interrupt = harn_vm::op_interrupt::install(Some(cancel), None);
            let request = dict(&[
                (
                    "symbol_ref",
                    dict(&[
                        ("name", vm_string("Reading")),
                        ("path", vm_string("src/reading.rs")),
                        ("kind", vm_string("Type")),
                    ]),
                ),
                ("new_name", vm_string("Measurement")),
                ("scope", vm_string("workspace")),
                ("backend", vm_string("syntax_tree")),
            ]);
            crate::code_index::rename::run(&shared, &[request]).expect("rename runs")
        }
    });
    // Let the rename reach the lease; a slow host only weakens the check.
    std::thread::sleep(std::time::Duration::from_millis(200));
    cancel.store(true, Ordering::SeqCst);
    drop(held);
    let result = renamer.join().unwrap();

    assert_eq!(s(field(&result, "result")), "cancelled", "{result:?}");
    assert_eq!(snapshot(fixture.workspace.path()), before);
}

/// Never answers `initialize`.
const NEVER_INITIALIZES: &str = "#!/bin/sh\nexec cat 3>&1 > /dev/null\n";

#[test]
fn a_deadline_during_server_startup_still_offers_the_alternative() {
    let _env = crate::process::test_support::declare_inherited();
    let fixture = fixture_with(NEVER_INITIALIZES, "");
    let before = snapshot(fixture.workspace.path());
    let result = rename(
        &fixture,
        &[
            ("backend", vm_string("language_server")),
            ("deadline_ms", VmValue::Int(300)),
        ],
    );

    assert_eq!(s(field(&result, "result")), "refused", "{result:?}");
    assert_eq!(
        s(field(field(&result, "alternative"), "backend")),
        "syntax_tree",
        "{result:?}"
    );
    assert!(
        matches!(field(&result, "retry_after_ms"), VmValue::Int(ms) if *ms >= 1_000),
        "{result:?}"
    );
    assert_eq!(snapshot(fixture.workspace.path()), before);
}
