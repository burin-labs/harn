//! How a command reports a fatal error and leaves the process — the shared
//! vocabulary the rest of the crate exits through.

use crate::*;

pub(crate) fn command_error(message: &str) -> ! {
    Cli::command()
        .error(ErrorKind::ValueValidation, message)
        .exit()
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
