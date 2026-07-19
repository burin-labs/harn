use clap::Parser as ClapParser;
use std::{panic, process};

use crate::cli::{Cli, Command};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CliRuntimeMode {
    FullIo,
    StaticAnalysis,
}

impl CliRuntimeMode {
    pub(crate) fn enables_tokio_io(self) -> bool {
        matches!(self, Self::FullIo)
    }
}

pub(crate) fn cli_runtime_mode(raw_args: &[String]) -> CliRuntimeMode {
    if raw_args.len() == 2 && raw_args[1].ends_with(".harn") {
        return CliRuntimeMode::FullIo;
    }

    let Ok(cli) = Cli::try_parse_from(raw_args) else {
        return CliRuntimeMode::StaticAnalysis;
    };

    if cli.json_schemas {
        return CliRuntimeMode::StaticAnalysis;
    }

    match cli.command {
        Some(
            Command::Check(_)
            | Command::Lint(_)
            | Command::Fmt(_)
            | Command::Parse(_)
            | Command::Tokens(_),
        ) => CliRuntimeMode::StaticAnalysis,
        _ => CliRuntimeMode::FullIo,
    }
}

pub(crate) fn build_cli_runtime(mode: CliRuntimeMode) -> tokio::runtime::Runtime {
    let build = panic::catch_unwind(|| {
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        if mode.enables_tokio_io() {
            builder.enable_all();
        } else {
            // Static analysis commands use synchronous file/parser paths.
            // Avoiding Tokio's I/O driver also avoids the Unix signal/process
            // self-pipe that sandboxed child processes may be forbidden to
            // create.
            builder.enable_time();
        }
        builder.build()
    });

    match build {
        Ok(Ok(runtime)) => runtime,
        Ok(Err(error)) => {
            eprintln!("failed to start async runtime: {error}");
            process::exit(1);
        }
        Err(payload) => {
            if let Some(message) = panic_payload_message(payload.as_ref())
                .filter(|message| message.contains("failed to create UnixStream"))
            {
                eprintln!(
                    "failed to start async runtime: Tokio signal driver unavailable in this environment: {message}"
                );
                if mode.enables_tokio_io() {
                    eprintln!(
                        "error: this harn command requires Tokio I/O support; run it outside a sandbox that denies socketpair"
                    );
                }
                process::exit(1);
            }
            panic::resume_unwind(payload);
        }
    }
}

pub(crate) fn exit_on_error(exit_code: i32) {
    if exit_code != 0 {
        process::exit(exit_code);
    }
}

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> Option<&str> {
    if let Some(message) = payload.downcast_ref::<String>() {
        Some(message.as_str())
    } else if let Some(message) = payload.downcast_ref::<&str>() {
        Some(message)
    } else {
        None
    }
}

pub(crate) fn is_broken_pipe_panic_payload(payload: &(dyn std::any::Any + Send)) -> bool {
    let Some(message) = panic_payload_message(payload) else {
        return false;
    };

    let print_failure = message.contains("failed printing to stdout")
        || message.contains("failed printing to stderr");
    let broken_pipe = message.contains("Broken pipe")
        || message.contains("os error 32")
        || message.contains("EPIPE");
    print_failure && broken_pipe
}

#[cfg(test)]
mod tests {
    use super::{cli_runtime_mode, is_broken_pipe_panic_payload, CliRuntimeMode};
    use crate::normalize_serve_args;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_string()).collect()
    }

    #[test]
    fn static_analysis_commands_use_runtime_without_tokio_io_driver() {
        for args in [
            argv(&["harn", "check", "script.harn"]),
            argv(&["harn", "lint", "script.harn"]),
            argv(&["harn", "fmt", "--check", "script.harn"]),
            argv(&["harn", "parse", "script.harn"]),
            argv(&["harn", "tokens", "script.harn"]),
            argv(&["harn", "--json-schemas"]),
        ] {
            assert_eq!(cli_runtime_mode(&args), CliRuntimeMode::StaticAnalysis);
        }
    }

    #[test]
    fn execution_commands_keep_full_tokio_io_runtime() {
        for args in [
            argv(&["harn", "run", "script.harn"]),
            argv(&["harn", "script.harn"]),
            argv(&["harn", "serve", "a2a", "script.harn"]),
            argv(&["harn", "test", "conformance"]),
        ] {
            let normalized = normalize_serve_args(args);
            assert_eq!(cli_runtime_mode(&normalized), CliRuntimeMode::FullIo);
        }
    }

    #[test]
    fn broken_pipe_print_panic_is_classified_as_clean_consumer_close() {
        let payload = String::from("failed printing to stdout: Broken pipe (os error 32)");
        assert!(is_broken_pipe_panic_payload(&payload));
    }

    #[test]
    fn unrelated_panic_is_not_classified_as_broken_pipe() {
        let payload = String::from("assertion failed: expected true");
        assert!(!is_broken_pipe_panic_payload(&payload));
    }
}
