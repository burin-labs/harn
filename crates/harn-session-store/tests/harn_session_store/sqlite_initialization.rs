use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use harn_session_store::{CreateSession, SessionStore, SqliteSessionStore};
use tempfile::TempDir;
use wait_timeout::ChildExt;

const PROCESS_TEST_DATABASE: &str = "HARN_SESSION_STORE_PROCESS_TEST_DATABASE";
const PROCESS_TEST_SESSION_ID: &str = "HARN_SESSION_STORE_PROCESS_TEST_SESSION_ID";
const CHILD_TIMEOUT: Duration = Duration::from_secs(10);

#[test]
fn sqlite_store_process_child() {
    let Some(database) = std::env::var_os(PROCESS_TEST_DATABASE) else {
        return;
    };
    let session_id = std::env::var(PROCESS_TEST_SESSION_ID).expect("child session id");

    println!("READY");
    std::io::stdout().flush().expect("flush ready signal");
    let mut release = [0_u8; 1];
    std::io::stdin()
        .read_exact(&mut release)
        .expect("read release signal");

    let store = SqliteSessionStore::open(&database).expect("open shared session store");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build child runtime");
    let (created, described) = runtime.block_on(async {
        let created = store
            .create(CreateSession {
                id: Some(session_id.clone()),
                ..CreateSession::default()
            })
            .await
            .expect("create child session");
        let described = store
            .describe(&session_id)
            .await
            .expect("read child session");
        (created, described)
    });
    assert_eq!(described, created);
    println!("DONE");
}

#[test]
fn sqlite_first_open_is_serialized_across_processes() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("first-open.sqlite");
    let mut children = vec![
        spawn_store_child(&path, "first-open-a"),
        spawn_store_child(&path, "first-open-b"),
    ];
    run_store_children(&mut children);

    let connection = rusqlite::Connection::open(path).expect("inspect shared session store");
    let journal_mode = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
        .expect("journal mode")
        .to_ascii_lowercase();
    let schema_version = connection
        .query_row(
            "SELECT version FROM _harn_sqlite_schema_versions WHERE name = 'session_store'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("session-store schema marker");
    let session_ids = connection
        .prepare("SELECT id FROM sessions ORDER BY id")
        .expect("prepare session query")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query sessions")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect sessions");
    assert_eq!(
        (journal_mode.as_str(), schema_version, session_ids),
        (
            "wal",
            2,
            vec!["first-open-a".to_string(), "first-open-b".to_string()]
        )
    );
}

struct StoreTestChild {
    child: Child,
    stdout: Option<BufReader<std::process::ChildStdout>>,
}

fn spawn_store_child(database: &Path, session_id: &str) -> StoreTestChild {
    let mut child = Command::new(std::env::current_exe().expect("current test executable"))
        .arg("sqlite_store_process_child")
        .arg("--nocapture")
        .env(PROCESS_TEST_DATABASE, database)
        .env(PROCESS_TEST_SESSION_ID, session_id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn session-store child process");
    let stdout = BufReader::new(child.stdout.take().expect("child stdout"));
    StoreTestChild {
        child,
        stdout: Some(stdout),
    }
}

fn run_store_children(children: &mut [StoreTestChild]) {
    for child in children.iter_mut() {
        read_store_child_until(child, "READY");
    }
    for child in children.iter_mut() {
        child
            .child
            .stdin
            .as_mut()
            .expect("child stdin")
            .write_all(b"initialize\n")
            .expect("release child initializer");
    }
    for child in children.iter_mut() {
        read_store_child_until(child, "DONE");
        let Some(status) = child
            .child
            .wait_timeout(CHILD_TIMEOUT)
            .expect("wait for session-store child")
        else {
            kill_and_reap(&mut child.child);
            panic!("session-store child did not exit within {CHILD_TIMEOUT:?}");
        };
        assert_eq!(status.code(), Some(0), "session-store child status");
    }
}

fn read_store_child_until(child: &mut StoreTestChild, marker: &str) {
    let mut reader = child.stdout.take().expect("session-store child stdout");
    let expected_marker = marker.to_string();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let reader_thread = std::thread::spawn(move || {
        let result = loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    break Err(format!(
                        "session-store child exited before emitting {expected_marker}"
                    ))
                }
                Ok(_) if line.split_whitespace().last() == Some(expected_marker.as_str()) => {
                    break Ok(());
                }
                Ok(_) => {}
                Err(error) => break Err(format!("could not read session-store child: {error}")),
            }
        };
        let _ = sender.send((reader, result));
    });
    match receiver.recv_timeout(CHILD_TIMEOUT) {
        Ok((reader, result)) => {
            child.stdout = Some(reader);
            reader_thread
                .join()
                .expect("join session-store output reader");
            result.unwrap_or_else(|error| panic!("{error}"));
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            kill_and_reap(&mut child.child);
            reader_thread
                .join()
                .expect("join timed-out session-store output reader");
            panic!("session-store child did not emit {marker} within {CHILD_TIMEOUT:?}");
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            reader_thread
                .join()
                .expect("join failed session-store output reader");
            panic!("session-store output reader disconnected before emitting {marker}");
        }
    }
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}
