// POSIX-bound: every test asserts on graceful-shutdown behaviour after sending
// SIGTERM to the orchestrator child (drain semantics, `graceful shutdown
// complete` log line, successful exit code, in-flight HTTP request drain).
// Windows lacks a portable signal delivery mechanism for console children, so
// the orchestrator cannot be drained the same way under test. The Windows
// nightly nextest matrix in `.github/workflows/windows-nightly.yml` runs
// every other test.
//
// See `docs/dev/windows-test-coverage.md` for the full inventory and
// disposition tracker (issue #946).
#![cfg(unix)]

#[path = "orchestrator_http/support.rs"]
mod support;
mod test_util;

#[path = "orchestrator_http/a2a.rs"]
mod a2a;
#[path = "orchestrator_http/admin.rs"]
mod admin;
#[path = "orchestrator_http/github.rs"]
mod github;
#[path = "orchestrator_http/notion.rs"]
mod notion;
#[path = "orchestrator_http/observability.rs"]
mod observability;
#[path = "orchestrator_http/slack.rs"]
mod slack;
