//! `harn precompile` — dispatches the directory-walk + per-file fanout
//! to the embedded `cli/precompile.harn` script.
//!
//! The .harn port owns argv parsing, walking, --out path mirroring, and
//! the per-file progress and summary render. The actual parse, typecheck,
//! and compile work stays in Rust behind a command-specific internal mode:
//! the script spawns `harn precompile <single-file>` per source with
//! `HARN_PRECOMPILE_INNER=1` so the child compiles one source instead of
//! recursing back into the directory walker.
//!
//! Phase deferrals: `harn time` and `harn bench` (the other two W13
//! commands) stay Rust-only in this PR — both depend on in-process VM
//! thread-locals (LLM trace summary, profile spans, `getrusage` CPU
//! samples) that don't survive a `harness.process.run` subprocess boundary
//! without inventing a new child-binary emit protocol. The W13 ticket
//! description presumed an `--internal-phase-emit` protocol on `harn
//! run` that doesn't actually exist in the current codebase; the
//! preconditions for porting each are filed as #2348 (`harn bench` →
//! `--emit-summary-json`) and #2350 (`harn time` → `--emit-phase-json`).
use harn_vm::bytecode_cache::{CACHE_EXTENSION, MODULE_CACHE_EXTENSION};
use std::path::{Path, PathBuf};

use harn_parser::DiagnosticSeverity;
use harn_vm::module_artifact::ModuleArtifact;

use crate::cli::PrecompileArgs;
use crate::command_error;
use crate::commands::collect_harn_files;
use crate::compiler_context::{imported_symbols_for_source, SourceCompilerAuthority};
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;
use crate::parse_source_file;
use crate::typecheck_imports::checker_with_resolved_imports;

mod artifact_path;
use artifact_path::output_path;

/// Env var the embedded `cli/precompile` script reads to find the
/// running `harn` binary path. Set from `std::env::current_exe()` so
/// the child invocation is robust to $PATH ordering / test sandboxes.
pub const PRECOMPILE_BIN_ENV: &str = "HARN_CLI_SELF_EXE";
pub const PRECOMPILE_ARTIFACT_CONTRACT: &str = "harn.precompile.artifacts/relocatable-v1";

/// Output directory the script forwards to its per-file child via
/// `--out`. Cleared on drop so a follow-on invocation in the same
/// process sees a clean env.
const PRECOMPILE_OUT_ENV: &str = "HARN_PRECOMPILE_OUT";
const PRECOMPILE_KEEP_GOING_ENV: &str = "HARN_PRECOMPILE_KEEP_GOING";
const PRECOMPILE_QUIET_ENV: &str = "HARN_PRECOMPILE_QUIET";
pub const PRECOMPILE_INNER_ENV: &str = "HARN_PRECOMPILE_INNER";

pub async fn run(args: PrecompileArgs) {
    if args.artifact_contract {
        println!("{PRECOMPILE_ARTIFACT_CONTRACT}");
        return;
    }
    if std::env::var(PRECOMPILE_INNER_ENV).as_deref() == Ok("1") {
        run_inner_compile(args);
        return;
    }

    let exe = std::env::current_exe().unwrap_or_else(|error| {
        command_error(&format!("failed to resolve current executable: {error}"))
    });
    let exe_str = exe.to_string_lossy().into_owned();
    let _bin = ScopedEnvVar::set(PRECOMPILE_BIN_ENV, &exe_str);
    let _out = args
        .out
        .as_ref()
        .map(|p| ScopedEnvVar::set(PRECOMPILE_OUT_ENV, &p.to_string_lossy()));
    let _keep = if args.keep_going {
        Some(ScopedEnvVar::set(PRECOMPILE_KEEP_GOING_ENV, "1"))
    } else {
        None
    };
    let _quiet = if args.quiet {
        Some(ScopedEnvVar::set(PRECOMPILE_QUIET_ENV, "1"))
    } else {
        None
    };

    let target = args
        .target
        .as_ref()
        .unwrap_or_else(|| command_error("precompile target is required"));
    let argv = vec![target.to_string_lossy().into_owned()];
    // Use the no-sandbox dispatch: precompile's target is whatever path
    // the user passed, which is typically outside the script's
    // tempfile-derived workspace root. The actual compile work still
    // runs inside the spawned child's default sandbox; the orchestration
    // layer this script implements just needs to read directory entries.
    let exit = dispatch::dispatch_to_embedded_script_no_sandbox(
        "precompile",
        argv,
        /* json_mode */ false,
    )
    .await;
    if exit != 0 {
        std::process::exit(exit);
    }
}

/// Outcome aggregated across all sources walked in one invocation.
#[derive(Default)]
struct Stats {
    compiled: usize,
    failed: usize,
}

/// One file can be both an executable entry pipeline AND an imported
/// module. Precompile emits both so the runtime loader hits whichever
/// path the user takes.
struct PrecompileArtifacts {
    entry_chunk: harn_vm::Chunk,
    module_artifact: Option<ModuleArtifact>,
}

/// Rust compiler entrypoint used by the `.harn` directory-walk driver for
/// each source file.
pub fn run_inner_compile(args: PrecompileArgs) {
    let target = args
        .target
        .clone()
        .unwrap_or_else(|| command_error("precompile target is required"));
    if !target.exists() {
        command_error(&format!("target does not exist: {}", target.display()));
    }

    let (sources, mut source_root) = if target.is_dir() {
        let mut files = Vec::new();
        collect_harn_files(&target, &mut files);
        files.sort();
        files.dedup();
        let root = target.canonicalize().unwrap_or_else(|_| target.clone());
        (files, Some(root))
    } else {
        (vec![target.clone()], None)
    };
    if source_root.is_none() && args.relocatable {
        source_root = target.parent().map(Path::to_path_buf);
    }

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

    // Resolve imports like `execute`/`harn check` so a call to an imported
    // symbol that shadows a builtin is checked against the right signature.
    let authority = SourceCompilerAuthority::for_source(source_path);
    let checker = checker_with_resolved_imports(authority.typechecker(), source_path);

    let mut had_type_error = false;
    let mut messages = String::new();
    for diag in checker.check_with_source(&program, &source) {
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

    let artifacts = compile_artifacts(source_path, &source, &program, authority)?;
    let entry_key = if source_root.is_some() {
        harn_vm::bytecode_cache::CacheKey::from_relocatable_source(source_path, &source)
    } else {
        harn_vm::bytecode_cache::CacheKey::from_source(source_path, &source)
    };

    let entry_dest = output_path(source_path, source_root, out_root, CACHE_EXTENSION)?;
    harn_vm::bytecode_cache::store_at(&entry_dest, &entry_key, &artifacts.entry_chunk)
        .map_err(|e| format!("write {}: {e}", entry_dest.display()))?;

    if let Some(module_artifact) = &artifacts.module_artifact {
        let module_source = harn_vm::module_source::ModuleSource::from_text(source.as_str());
        let module_key = harn_vm::bytecode_cache::CacheKey::from_module_source(&module_source);
        let module_dest = output_path(source_path, source_root, out_root, MODULE_CACHE_EXTENSION)?;
        harn_vm::bytecode_cache::store_module_at(&module_dest, &module_key, module_artifact)
            .map_err(|e| format!("write {}: {e}", module_dest.display()))?;
    }

    Ok(entry_dest)
}

/// Compile both the entry-chunk view and the module-artifact view of the
/// same source. A `.harn` file with a `pipeline default { ... }` block is
/// callable as both an entry and an importable module; one without is
/// importable but produces an entry chunk that just returns `nil`. We
/// emit both artifacts unconditionally so the runtime loader hits the
/// cache regardless of how the user invokes the file.
fn compile_artifacts(
    source_path: &Path,
    source: &str,
    program: &[harn_parser::SNode],
    authority: SourceCompilerAuthority,
) -> Result<PrecompileArtifacts, String> {
    let imported = imported_symbols_for_source(source_path, source);
    let entry_chunk = authority
        .compiler_with_imported_symbols(
            imported.enum_candidates.iter().cloned(),
            imported.callable_names.iter().cloned(),
        )
        .compile(program)
        .map_err(|e| format!("compile error: {e}"))?;
    let module_artifact = authority
        .compile_module_with_imported_symbols(
            source_path,
            source,
            imported.enum_candidates,
            imported.callable_names,
        )
        .map_err(|e| format!("module compile error: {e}"))
        .ok();
    Ok(PrecompileArtifacts {
        entry_chunk,
        module_artifact,
    })
}
