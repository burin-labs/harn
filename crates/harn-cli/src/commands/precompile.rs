//! Implementation of the `harn precompile` subcommand.
//!
//! Walks a file or directory tree, compiles every `.harn` file into a
//! `.harnbc` artifact, and either writes it adjacent to the source or
//! mirrors the directory layout under `--out`. The on-disk artifact is
//! the same format the runtime cache writes, so a shipped `.harnbc`
//! beside its source elides parse+compile in the runtime loader.

use std::path::{Path, PathBuf};

use harn_parser::DiagnosticSeverity;

use crate::cli::PrecompileArgs;
use crate::command_error;
use crate::commands::collect_harn_files;
use crate::parse_source_file;

/// Outcome aggregated across all sources walked in one invocation.
#[derive(Default)]
struct Stats {
    compiled: usize,
    failed: usize,
}

pub fn run(args: PrecompileArgs) {
    let target = args.target.clone();
    if !target.exists() {
        command_error(&format!("target does not exist: {}", target.display()));
    }

    let (sources, source_root) = if target.is_dir() {
        let mut files = Vec::new();
        collect_harn_files(&target, &mut files);
        files.sort();
        files.dedup();
        let root = target.canonicalize().unwrap_or_else(|_| target.clone());
        (files, Some(root))
    } else {
        (vec![target.clone()], None)
    };

    if sources.is_empty() {
        command_error(&format!("no .harn files found under {}", target.display()));
    }

    let mut stats = Stats::default();
    for source in &sources {
        let result = precompile_one(source, source_root.as_deref(), args.out.as_deref());
        match result {
            Ok(out_path) => {
                stats.compiled += 1;
                if !args.quiet {
                    println!("{} -> {}", source.display(), out_path.display());
                }
            }
            Err(err) => {
                stats.failed += 1;
                eprintln!("{}: {err}", source.display());
                if !args.keep_going {
                    break;
                }
            }
        }
    }

    if !args.quiet {
        eprintln!(
            "precompile: {} succeeded, {} failed",
            stats.compiled, stats.failed
        );
    }
    if stats.failed > 0 {
        std::process::exit(1);
    }
}

fn precompile_one(
    source_path: &Path,
    source_root: Option<&Path>,
    out_root: Option<&Path>,
) -> Result<PathBuf, String> {
    let source = std::fs::read_to_string(source_path).map_err(|e| format!("read: {e}"))?;
    let path_str = source_path.to_string_lossy();

    let (parsed_source, program) = parse_source_file(&path_str);
    debug_assert_eq!(parsed_source, source);

    let mut had_type_error = false;
    let mut messages = String::new();
    for diag in harn_parser::TypeChecker::new().check_with_source(&program, &source) {
        let rendered = harn_parser::diagnostic::render_type_diagnostic(&source, &path_str, &diag);
        if matches!(diag.severity, DiagnosticSeverity::Error) {
            had_type_error = true;
        }
        messages.push_str(&rendered);
    }
    if had_type_error {
        return Err(format!("type errors:\n{messages}"));
    }
    if !messages.is_empty() {
        eprint!("{messages}");
    }

    let chunk = harn_vm::Compiler::new()
        .compile(&program)
        .map_err(|e| format!("compile error: {e}"))?;
    let key = harn_vm::bytecode_cache::CacheKey::from_source(source_path, &source);

    let dest = output_path(source_path, source_root, out_root)?;
    harn_vm::bytecode_cache::store_at(&dest, &key, &chunk)
        .map_err(|e| format!("write {}: {e}", dest.display()))?;

    Ok(dest)
}

/// Map a source path under (optional) `source_root` to its destination
/// under (optional) `out_root`. When no `out_root` is given the artifact
/// lands adjacent to the source.
fn output_path(
    source_path: &Path,
    source_root: Option<&Path>,
    out_root: Option<&Path>,
) -> Result<PathBuf, String> {
    let adjacent = harn_vm::bytecode_cache::adjacent_cache_path(source_path)
        .ok_or_else(|| format!("source has no file stem: {}", source_path.display()))?;
    let Some(out_root) = out_root else {
        return Ok(adjacent);
    };
    let relative = match source_root {
        Some(root) => {
            let canonical = source_path
                .canonicalize()
                .unwrap_or_else(|_| source_path.to_path_buf());
            canonical
                .strip_prefix(root)
                .map(Path::to_path_buf)
                .unwrap_or_else(|_| {
                    PathBuf::from(source_path.file_name().unwrap_or(source_path.as_os_str()))
                })
        }
        None => PathBuf::from(
            source_path
                .file_name()
                .ok_or_else(|| format!("source has no file name: {}", source_path.display()))?,
        ),
    };
    let mut dest = out_root.join(&relative);
    dest.set_extension(harn_vm::bytecode_cache::CACHE_EXTENSION);
    Ok(dest)
}
