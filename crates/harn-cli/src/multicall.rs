//! The multi-call entry point shared by every executable that carries Harn.
//!
//! `harn` ships as one binary; `harn-lsp` and `harn-dap` are symlinks (Unix)
//! or copies (Windows) of it. [`main`] dispatches on the name the process was
//! invoked as (`argv[0]`) so editors that spawn `harn-lsp` / `harn-dap` by path
//! keep working unchanged, while a release ships one binary instead of three
//! near-identical statically-linked copies. A host program that embeds Harn can
//! call [`main`] when [`invoked_as`] names one of [`NAMES`] and so carry the
//! whole toolchain in its own executable.

use std::ffi::OsStr;

/// Program names [`main`] answers to.
pub const NAMES: [&str; 3] = ["harn", "harn-lsp", "harn-dap"];

/// Initialize the Harn runtime and run the CLI, language server or debug adapter
/// that `argv[0]` selects. Returns when that program finishes; failures exit
/// the process with their own status.
pub fn main() {
    #[cfg(feature = "hostlib")]
    if harn_hostlib::process::owner_death::run_if_requested() {
        unreachable!("owner-death guardian execution does not return");
    }
    // Load stdlib imports from the artifacts compiled into this binary, so a
    // first run on a new machine compiles no stdlib module (harn#9652).
    // A refused table only costs compile time; the release smoke gates on it.
    let _ = harn_stdlib_artifacts::register();
    // The compiler and runtime read the stdlib through the catalog this binary
    // installs, so a stdlib edit rebuilds only harn-stdlib and this binary.
    if let Err(error) = harn_stdlib::install() {
        cli_diagnostic!("error: {error}");
        std::process::exit(2);
    }
    match harn_vm::initialize_runtime() {
        Err(error) => {
            cli_diagnostic!("error: {error}");
            std::process::exit(2);
        }
        // Exactly one line, once, naming why caching is off for this run.
        Ok(Some(warning)) => cli_diagnostic!("warning: {warning}"),
        Ok(None) => {}
    }
    match invoked_as().as_deref() {
        Some("harn-lsp") => run_sidecar("harn-lsp", harn_lsp::run),
        Some("harn-dap") => run_sidecar("harn-dap", harn_dap::run),
        _ => crate::run(),
    }
}

/// The program name `harn` was invoked as, derived from `argv[0]`'s file stem
/// so it matches whether launched as `harn-lsp`, `/usr/local/bin/harn-lsp`, or
/// `harn-lsp.exe`. Returns `None` if `argv[0]` is absent or non-UTF-8, in which
/// case the caller falls through to the default CLI.
pub fn invoked_as() -> Option<String> {
    std::env::args_os()
        .next()
        .as_deref()
        .map(std::path::Path::new)
        .and_then(std::path::Path::file_stem)
        .and_then(|stem| stem.to_str())
        .map(str::to_owned)
}

/// Run a stdio sidecar (`harn-lsp` / `harn-dap`), but answer a bare
/// `--version`/`-V` or `--help`/`-h` probe first.
///
/// The sidecars speak a wire protocol over stdin/stdout and don't otherwise
/// parse argv, so without this a `--version` probe would silently start the
/// server and hang waiting on stdin. Keep this probe path aligned with the
/// standalone sidecar binaries. Regression fix for #2975.
fn run_sidecar(name: &str, serve: fn()) {
    match sidecar_query(std::env::args_os().skip(1)) {
        Some(SidecarQuery::Version) => println!("{name} {}", env!("CARGO_PKG_VERSION")),
        Some(SidecarQuery::Help) => print_sidecar_help(name),
        None => serve(),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SidecarQuery {
    Version,
    Help,
}

/// Detect a leading `--version`/`-V`/`--help`/`-h` probe in a sidecar's args.
///
/// Only the first argument is inspected: editors launch the sidecars with no
/// arguments, so anything else falls through to the server unchanged rather
/// than risk intercepting a future protocol-relevant flag.
fn sidecar_query<I, S>(mut args: I) -> Option<SidecarQuery>
where
    I: Iterator<Item = S>,
    S: AsRef<OsStr>,
{
    match args.next()?.as_ref().to_str()? {
        "--version" | "-V" => Some(SidecarQuery::Version),
        "--help" | "-h" => Some(SidecarQuery::Help),
        _ => None,
    }
}

fn print_sidecar_help(name: &str) {
    let kind = if name.ends_with("dap") {
        "debug adapter (DAP)"
    } else {
        "language server (LSP)"
    };
    println!("{name} {}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("The Harn {kind}. It communicates over stdio and is normally");
    println!("launched by an editor rather than run directly.");
    println!();
    println!("Options:");
    println!("  -V, --version    Print version and exit");
    println!("  -h, --help       Print this help and exit");
}

#[cfg(test)]
mod tests {
    use super::{sidecar_query, SidecarQuery};

    #[test]
    fn detects_version_flags() {
        assert_eq!(
            sidecar_query(["--version"].iter()),
            Some(SidecarQuery::Version)
        );
        assert_eq!(sidecar_query(["-V"].iter()), Some(SidecarQuery::Version));
    }

    #[test]
    fn detects_help_flags() {
        assert_eq!(sidecar_query(["--help"].iter()), Some(SidecarQuery::Help));
        assert_eq!(sidecar_query(["-h"].iter()), Some(SidecarQuery::Help));
    }

    #[test]
    fn no_args_runs_server() {
        let empty: [&str; 0] = [];
        assert_eq!(sidecar_query(empty.iter()), None);
    }

    #[test]
    fn unrelated_first_arg_runs_server() {
        // A non-query leading arg falls through to the server untouched.
        assert_eq!(sidecar_query(["--stdio"].iter()), None);
        assert_eq!(sidecar_query(["serve", "--version"].iter()), None);
    }
}
