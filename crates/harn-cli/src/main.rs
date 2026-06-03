//! Entry point for the single multi-call `harn` binary.
//!
//! `harn` ships as one binary; the `harn-lsp` and `harn-dap` artifacts are
//! symlinks (Unix) or copies (Windows) of it. We dispatch on the name the
//! binary was invoked as (`argv[0]`) so editors that spawn `harn-lsp` /
//! `harn-dap` by path keep working unchanged, while the release ships one
//! binary instead of three near-identical statically-linked copies.

use std::ffi::OsStr;

fn main() {
    match invoked_as().as_deref() {
        Some("harn-lsp") => run_sidecar("harn-lsp", harn_lsp::run),
        Some("harn-dap") => run_sidecar("harn-dap", harn_dap::run),
        _ => harn_cli::run(),
    }
}

/// The program name `harn` was invoked as, derived from `argv[0]`'s file stem
/// so it matches whether launched as `harn-lsp`, `/usr/local/bin/harn-lsp`, or
/// `harn-lsp.exe`. Returns `None` if `argv[0]` is absent or non-UTF-8, in which
/// case the caller falls through to the default CLI.
fn invoked_as() -> Option<String> {
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
/// server and hang waiting on stdin. As standalone binaries they used to
/// answer `--version`; the multi-call collapse dropped that. Regression fix
/// for #2975.
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
