//! Regression coverage for attribute-decorated top-level declarations.
//!
//! Defining any attribute-decorated top-level function — `@route`,
//! `@deprecated`, … on a `fn`/`pub fn` — used to crash at runtime with
//! "Stack underflow", even when the function was never called. In
//! script-mode compilation (a file with a top-level `fn main(...)`
//! entrypoint) the top-level loop emits an `Op::Pop` after any item for
//! which `produces_value` is true; a bare `Node::FnDecl` correctly
//! reports `false`, but a `Node::AttributedDecl` wrapping it fell through
//! to the `_ => true` catch-all, so the compiler popped against an empty
//! operand stack. This broke `harn serve site`, whose routed handlers are
//! all `@route`-decorated `pub fn`s.
//!
//! Each case runs a real script end-to-end through `harn run` and asserts
//! it exits cleanly — a stack-balance failure surfaces only at runtime, so
//! a compile-only test would not catch the regression. These are
//! binary-surface tests and follow the repo convention of `#[ignore]`
//! (the in-process `harn serve site` demo in `demo_cli.rs` is the fast CI
//! gate for the same code path).

use crate::test_util;

use std::fs;

use tempfile::TempDir;
use test_util::process::harn_e2e_command;

fn run_script(body: &str) -> std::process::Output {
    let temp = TempDir::new().unwrap();
    let script = temp.path().join("main.harn");
    fs::write(&script, body).unwrap();
    harn_e2e_command()
        .current_dir(temp.path())
        .args(["run", script.to_str().unwrap()])
        .output()
        .unwrap()
}

fn assert_ok_with_stdout(body: &str, needle: &str) {
    let out = run_script(body);
    assert_eq!(
        out.status.code(),
        Some(0),
        "expected clean exit, got {:?}\nstdout={}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(needle),
        "expected {needle:?} in stdout, got: {stdout}"
    );
}

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn route_decorated_uncalled_fn_does_not_underflow() {
    assert_ok_with_stdout(
        r#"
@route("GET", "/hello")
pub fn hello(req: dict) -> dict { return { status: 200 } }

fn main(harness: Harness) { __io_println("ok") }
"#,
        "ok",
    );
}

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn deprecated_decorated_pub_fn_does_not_underflow() {
    assert_ok_with_stdout(
        r#"
@deprecated
pub fn f(x: int) -> int { return x + 1 }

fn main(harness: Harness) { __io_println("ok") }
"#,
        "ok",
    );
}

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn deprecated_decorated_non_pub_fn_does_not_underflow() {
    assert_ok_with_stdout(
        r#"
@deprecated
fn f(x: int) -> int { return x + 1 }

fn main(harness: Harness) { __io_println("ok") }
"#,
        "ok",
    );
}

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn route_decorated_fn_can_be_called_from_main() {
    assert_ok_with_stdout(
        r#"
@route("GET", "/hello")
pub fn hello(req: dict) -> dict { return { status: 200 } }

fn main(harness: Harness) {
    const resp = hello({})
    if resp.status == 200 {
        __io_println("ok")
    }
}
"#,
        "ok",
    );
}
