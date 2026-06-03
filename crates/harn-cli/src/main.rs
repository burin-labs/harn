//! Entry point for the single multi-call `harn` binary.
//!
//! `harn` ships as one binary; the `harn-lsp` and `harn-dap` artifacts are
//! symlinks (Unix) or copies (Windows) of it. We dispatch on the name the
//! binary was invoked as (`argv[0]`) so editors that spawn `harn-lsp` /
//! `harn-dap` by path keep working unchanged, while the release ships one
//! binary instead of three near-identical statically-linked copies.

fn main() {
    match invoked_as().as_deref() {
        Some("harn-lsp") => harn_lsp::run(),
        Some("harn-dap") => harn_dap::run(),
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
