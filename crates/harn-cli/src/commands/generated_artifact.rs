//! Shared "write, or `--check` for drift" plumbing for commands that render a
//! committed artifact. Each generator owns its rendering; this module owns
//! how the result is compared with disk, reported, and written.

use std::fs;
use std::path::Path;
use std::process;

use crate::format::normalize_line_endings;

/// Write `generated` to `path`, or with `check_only` compare it (ignoring
/// line endings) with the file on disk and exit 1 when it is missing or
/// stale. `source` names what the artifact is derived from and `make_target`
/// is the regeneration target named in the hint.
pub(crate) fn write_or_check(
    path: &Path,
    generated: &str,
    check_only: bool,
    source: &str,
    make_target: &str,
) {
    if check_only {
        match fs::read_to_string(path) {
            Ok(existing)
                if normalize_line_endings(&existing) == normalize_line_endings(generated) =>
            {
                return;
            }
            Ok(_) => eprintln!("error: {} is stale relative to {source}.", path.display()),
            Err(error) => eprintln!("error: cannot read {}: {error}", path.display()),
        }
        eprintln!("hint: run `make {make_target}` to regenerate.");
        process::exit(1);
    }

    if let Err(error) = write_file(path, generated) {
        eprintln!("error: {error}");
        process::exit(1);
    }
    println!("wrote {}", path.display());
}

/// Compare `rendered` (ignoring line endings) with `out` for codegen
/// commands that report drift through their exit code. Returns 0 when
/// current and 1 (after reporting) when stale or missing.
pub(crate) fn check_against(out: &Path, rendered: &str, regenerate: &str) -> i32 {
    match fs::read_to_string(out) {
        Ok(existing) if normalize_line_endings(&existing) == normalize_line_endings(rendered) => 0,
        Ok(_) => {
            eprintln!(
                "{} is out of date; regenerate with `{regenerate}`",
                out.display()
            );
            1
        }
        Err(_) => {
            eprintln!(
                "{} is missing; generate it with `{regenerate}`",
                out.display()
            );
            1
        }
    }
}

/// True when `path` holds exactly `expected`.
pub(crate) fn is_current(path: &Path, expected: &str) -> bool {
    matches!(fs::read_to_string(path), Ok(existing) if existing == expected)
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
