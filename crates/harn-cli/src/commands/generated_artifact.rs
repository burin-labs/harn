//! Shared "write, or `--check` for drift" plumbing for commands that render a
//! committed artifact. Each generator owns its rendering; this module owns
//! how the result is compared with disk, reported, and written.

use std::fs;
use std::path::Path;
use std::process;

use crate::format::normalize_line_endings;

/// Write `generated` to `path`, or with `check_only` exit 1 when the file on
/// disk is missing or stale. An already-current file is left untouched, so a
/// regeneration does not bump mtimes that downstream build gates compare.
/// `source` names what the artifact is derived from and `make_target` is the
/// regeneration target named in the hint.
pub(crate) fn write_or_check(
    path: &Path,
    generated: &str,
    check_only: bool,
    source: &str,
    make_target: &str,
) {
    if check_only {
        check_or_exit(path, generated, source, make_target);
        return;
    }
    if is_current(path, generated) {
        return;
    }
    if let Err(error) = write_file(path, generated) {
        cli_diagnostic!("error: {error}");
        process::exit(1);
    }
    println!("wrote {}", path.display());
}

/// Exit 1, naming the regeneration target, unless `path` already holds
/// `generated`.
pub(crate) fn check_or_exit(path: &Path, generated: &str, source: &str, make_target: &str) {
    match fs::read_to_string(path) {
        Ok(existing) if same_text(&existing, generated) => return,
        Ok(_) => cli_diagnostic!("error: {} is stale relative to {source}.", path.display()),
        Err(error) => cli_diagnostic!("error: cannot read {}: {error}", path.display()),
    }
    cli_diagnostic!("hint: run `make {make_target}` to regenerate.");
    process::exit(1);
}

/// Compare `rendered` with `out` for codegen commands that report drift
/// through their exit code. Returns 0 when current and 1 (after reporting)
/// when stale or missing.
pub(crate) fn check_against(out: &Path, rendered: &str, regenerate: &str) -> i32 {
    match fs::read_to_string(out) {
        Ok(existing) if same_text(&existing, rendered) => 0,
        Ok(_) => {
            cli_diagnostic!(
                "{} is out of date; regenerate with `{regenerate}`",
                out.display()
            );
            1
        }
        Err(_) => {
            cli_diagnostic!(
                "{} is missing; generate it with `{regenerate}`",
                out.display()
            );
            1
        }
    }
}

/// True when `path` holds `expected`. Every comparison in this module ignores
/// line endings, so a CRLF checkout of a committed artifact is not drift.
pub(crate) fn is_current(path: &Path, expected: &str) -> bool {
    matches!(fs::read_to_string(path), Ok(existing) if same_text(&existing, expected))
}

fn same_text(existing: &str, generated: &str) -> bool {
    normalize_line_endings(existing) == normalize_line_endings(generated)
}

/// Write `body` to `path`, creating parent directories.
pub(crate) fn write_file(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    fs::write(path, body).map_err(|error| format!("cannot write {}: {error}", path.display()))
}
