use std::process;

use harn_fmt::{format_source_opts, FmtOptions};

/// Whether `harn fmt` should rewrite files in place or just report drift.
#[derive(Clone, Copy, Debug)]
pub(crate) enum FmtMode {
    /// Rewrite files that aren't already formatted.
    Write,
    /// Only report files that would be reformatted; never write to disk.
    Check,
}

impl FmtMode {
    pub(crate) fn from_check_flag(check: bool) -> Self {
        if check {
            Self::Check
        } else {
            Self::Write
        }
    }

    fn is_check(self) -> bool {
        matches!(self, Self::Check)
    }
}

/// Format one or more files or directories. Accepts multiple targets.
pub(crate) fn fmt_targets(targets: &[&str], mode: FmtMode, opts: &FmtOptions) {
    let mut files = Vec::new();
    for target in targets {
        let path = std::path::Path::new(target);
        if path.is_dir() {
            super::super::collect_harn_files(path, &mut files);
        } else {
            files.push(path.to_path_buf());
        }
    }
    if files.is_empty() {
        eprintln!("No .harn files found");
        process::exit(1);
    }
    let mut has_error = false;
    for file in &files {
        let path_str = file.to_string_lossy();
        if !fmt_file_inner(&path_str, mode, opts) {
            has_error = true;
        }
    }
    if has_error {
        process::exit(1);
    }
}

/// Format a single file. Returns true on success, false on error.
fn fmt_file_inner(path: &str, mode: FmtMode, opts: &FmtOptions) -> bool {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error reading {path}: {e}");
            return false;
        }
    };

    let formatted = match format_source_opts(&source, opts) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{path}: {e}");
            return false;
        }
    };

    if mode.is_check() {
        if source != formatted {
            eprintln!("{path}: would be reformatted");
            return false;
        }
    } else if source != formatted {
        match std::fs::write(path, &formatted) {
            Ok(()) => println!("formatted {path}"),
            Err(e) => {
                eprintln!("Error writing {path}: {e}");
                return false;
            }
        }
    }
    true
}
