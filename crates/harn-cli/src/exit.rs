//! How a command reports a fatal error and leaves the process — the shared
//! vocabulary the rest of the crate exits through.

use crate::*;

tokio::task_local! {
    static EXPLAIN_DIAGNOSTICS: bool;
}

/// Carry the parsed CLI option with this invocation, including across awaits.
pub(crate) async fn with_diagnostic_detail<F: std::future::Future>(
    explain: bool,
    future: F,
) -> F::Output {
    EXPLAIN_DIAGNOSTICS.scope(explain, future).await
}

pub(crate) fn diagnostic_detail_requested() -> bool {
    EXPLAIN_DIAGNOSTICS
        .try_with(|explain| *explain)
        .unwrap_or(false)
}

/// Keep ordinary command errors short while leaving non-error status intact.
pub(crate) fn log_cli_diagnostic(message: &str) {
    let is_error = message
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("error:"));
    if is_error && !diagnostic_detail_requested() && message.lines().count() > 1 {
        let summary = message.lines().next().unwrap_or("error").trim_end();
        harn_vm::events::log_status(&format!("{summary} (use --explain for details)"));
    } else {
        harn_vm::events::log_status(message);
    }
}

/// A run failed while Harn was preparing it, before the program ran.
///
/// Package materialization, manifest trigger and hook installation, provider
/// connector loading, and environment-policy launch all happen on the program's
/// behalf, not as part of it. Reporting them with the same status the program
/// itself fails with leaves a caller unable to ask the one question that decides
/// what to do next: did setup fail, or did the program fail? A dependency that
/// could not be prepared is retried or reported as infrastructure; a program
/// that returned a failure is a result.
///
/// A Harn program picks its own exit status, so no value here is unreachable by
/// a program that insists on returning it. What makes this one usable is that it
/// is reserved by contract: Harn documents it, never produces it for a program
/// outcome, and a caller may branch on it. The number follows the convention
/// `env(1)`, `timeout(1)`, and `docker run` already established, where 125 means
/// the harness failed rather than the command it was asked to run — the same
/// convention `RUN_INTERRUPTED` borrows for 124.
pub const RUN_SETUP_FAILURE: i32 = 125;

/// A run was stopped by an interrupt or a deadline rather than finishing.
///
/// Reserved on the same terms as [`RUN_SETUP_FAILURE`], and borrowed from
/// `timeout(1)` for the same reason.
pub const RUN_INTERRUPTED: i32 = 124;

/// The program ran and failed.
pub const PROGRAM_FAILURE: i32 = 1;

/// Whether a run failed while Harn was preparing it or while the program itself
/// was the thing that failed.
///
/// Every path that ends a run early classifies itself through this rather than
/// picking a status literal, so "which failures are infrastructure" stays one
/// decision instead of a convention each call site re-derives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunFailure {
    /// Harn could not prepare the run: locked dependencies, manifest triggers
    /// and hooks, provider connectors, or the session's environment policy.
    /// Nothing of the program has executed.
    Setup,
    /// The program is what failed — it did not compile, or it ran and returned
    /// a failure.
    Program,
}

impl RunFailure {
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Setup => RUN_SETUP_FAILURE,
            Self::Program => PROGRAM_FAILURE,
        }
    }
}

pub(crate) fn command_error(message: &str) -> ! {
    fatal_error_with_code(message, 2)
}

pub(crate) fn fatal_error(message: impl std::fmt::Display) -> ! {
    fatal_error_with_code(message, 1)
}

pub(crate) fn fatal_error_with_code(message: impl std::fmt::Display, code: i32) -> ! {
    log_cli_diagnostic(&format!("error: {message}"));
    process::exit(code);
}

pub(crate) fn print_check_error(code: &str, message: &str) -> ! {
    let envelope: json_envelope::JsonEnvelope<commands::check::CheckReport> =
        json_envelope::JsonEnvelope::err(commands::check::CHECK_SCHEMA_VERSION, code, message);
    println!("{}", json_envelope::to_string_pretty(&envelope));
    process::exit(1);
}

pub(crate) fn print_lint_error(code: &str, message: &str) -> ! {
    let envelope: json_envelope::JsonEnvelope<commands::check::LintReport> =
        json_envelope::JsonEnvelope::err(commands::check::LINT_SCHEMA_VERSION, code, message);
    println!("{}", json_envelope::to_string_pretty(&envelope));
    process::exit(1);
}
