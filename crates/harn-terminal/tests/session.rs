#![cfg(unix)]

use std::collections::BTreeMap;
use std::time::Duration;

use harn_terminal::{
    CellRegion, InputEvent, KeyCode, Modifier, NamedKey, ProcessStatus, SessionOptions,
    TerminalSession,
};

const TIMEOUT: Duration = Duration::from_secs(3);

fn shell(script: &str, rows: u16, cols: u16) -> TerminalSession {
    TerminalSession::spawn(SessionOptions {
        argv: vec!["sh".into(), "-c".into(), script.into()],
        rows,
        cols,
        cwd: None,
        env: BTreeMap::new(),
        env_remove: Vec::new(),
        raw_capacity: 4096,
    })
    .expect("spawn shell")
}

#[test]
fn captures_typed_screen_cursor_styles_and_wide_cells() {
    let session = shell("printf '\\033[2;3H\\033[1;31m界\\033[0m'", 4, 12);
    session.wait_exit(TIMEOUT).expect("shell exits");
    let capture = session
        .capture(Some(CellRegion {
            row: 1,
            column: 2,
            rows: 1,
            columns: 2,
        }))
        .expect("capture");

    assert_eq!(capture.rows, 4);
    assert_eq!(capture.columns, 12);
    assert_eq!(capture.cursor_row, 1);
    assert_eq!(capture.cursor_column, 4);
    assert_eq!(capture.text_rows.len(), 4);
    let cells = capture.cells.expect("detailed cells");
    assert_eq!(cells[0].text, "界");
    assert!(cells[0].bold);
    assert!(cells[0].wide);
    assert!(cells[1].wide_continuation);
}

#[test]
fn sends_control_keys_resizes_and_waits_without_polling() {
    let session = shell(
        "stty -echo; printf READY; IFS= read -r line; printf '\\nGOT:%s' \"$line\"",
        6,
        20,
    );
    session
        .wait_idle(Duration::from_millis(20), TIMEOUT)
        .expect("initial output settles");
    assert!(session
        .capture(None)
        .unwrap()
        .text_rows
        .join("\n")
        .contains("READY"));

    session.resize(8, 30).expect("resize");
    session
        .send(&[
            InputEvent::Text {
                text: "alpha beta".into(),
            },
            InputEvent::Key {
                key: KeyCode::Character { value: 'w' },
                modifiers: vec![Modifier::Ctrl],
            },
            InputEvent::Text {
                text: "gamma".into(),
            },
            InputEvent::Key {
                key: KeyCode::Named {
                    name: NamedKey::Enter,
                },
                modifiers: vec![],
            },
        ])
        .expect("send typed batch");
    let status = session.wait_exit(TIMEOUT).expect("shell exits");
    assert!(matches!(
        status,
        ProcessStatus::Exited { code: Some(0), .. }
    ));
    let capture = session.capture(None).expect("capture");
    assert_eq!((capture.rows, capture.columns), (8, 30));
    assert!(capture.text_rows.join("\n").contains("GOT:alpha gamma"));
}

#[test]
fn end_terminates_and_reaps_the_child() {
    let session = shell("while :; do read line || exit; done", 4, 12);
    let status = session.end(TIMEOUT).expect("end session");
    assert!(status.finished());
}

#[test]
fn idle_wait_handles_a_silent_exit() {
    let session = shell(":", 4, 12);
    let idle = session
        .wait_idle(Duration::from_millis(20), TIMEOUT)
        .expect("silent exit is a settled terminal");
    assert!(idle.status.finished());
}

#[test]
fn revision_fence_rejects_output_that_was_already_idle() {
    let session = shell(
        "stty -echo; printf READY; IFS= read -r line; printf 'GOT:%s' \"$line\"; cat",
        4,
        20,
    );
    session
        .wait_idle(Duration::from_millis(10), TIMEOUT)
        .expect("initial output settles");
    let baseline = session.capture(None).expect("baseline capture").revision;

    let error = session
        .wait_idle_after(
            baseline,
            Duration::from_millis(5),
            Duration::from_millis(20),
        )
        .expect_err("old quiet output must not cross a new revision fence");
    assert!(error.to_string().contains("timed out"));

    session
        .send(&[
            InputEvent::Text {
                text: "new-output".into(),
            },
            InputEvent::Key {
                key: KeyCode::Named {
                    name: NamedKey::Enter,
                },
                modifiers: vec![],
            },
        ])
        .expect("send input");
    session
        .wait_idle_after(baseline, Duration::from_millis(10), TIMEOUT)
        .expect("new output crosses the fence and settles");
    assert!(session
        .capture(None)
        .expect("capture")
        .text_rows
        .join("\n")
        .contains("GOT:new-output"));
    session.end(TIMEOUT).expect("end session");
}
