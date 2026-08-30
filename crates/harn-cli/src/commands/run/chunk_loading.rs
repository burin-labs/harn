//! Turning an entry path into a runnable [`harn_vm::Chunk`].
//!
//! The cache is the fast path and the compiler is the fallback, but both end at
//! the same place, so they live together rather than at opposite ends of the run
//! command. Parse and type-check sit here too: they are the phases the cache
//! exists to skip, and `harn run` calls them directly when it needs diagnostics
//! for a file it is not about to execute.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use harn_parser::DiagnosticSeverity;

use super::import_failure::{ImportFailureDetail, ImportLoadFailure};
use super::ProjectContextMode;
use crate::commands::time::RunTiming;
use crate::package;

/// Why an entry path did not become a runnable chunk.
///
/// The diagnostic text still carries the detail; this carries the one bit a
/// caller has to branch on. Preparing a run's dependencies is work Harn does on
/// the program's behalf, while a parse, type, or compile error is the program's
/// own content failing — and reporting both as "did not load" is what made a run
/// that could not materialize its packages indistinguishable from a run whose
/// program was broken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ChunkLoadFailure {
    /// The entry file could not be read.
    EntryUnreadable,
    /// The run's locked dependencies could not be materialized.
    PackageMaterialization,
    /// An import failed before the VM started. The typed facts are retained
    /// here so every run-launch projection consumes the same value.
    Import(ImportFailureDetail),
    /// The program did not parse, typecheck, or compile.
    Program,
}

impl ChunkLoadFailure {
    /// The `--json` error event's `code`. Program failures keep reporting
    /// `compile_error`; the preparation codes are new because there was
    /// previously nothing to name.
    pub(crate) fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::EntryUnreadable => "entry_unreadable",
            Self::PackageMaterialization => "package_materialization",
            Self::Import(_) => "compile_error",
            Self::Program => "compile_error",
        }
    }

    pub(crate) fn classification(&self) -> crate::exit::RunFailure {
        match self {
            Self::EntryUnreadable | Self::PackageMaterialization => crate::exit::RunFailure::Setup,
            Self::Import(_) | Self::Program => crate::exit::RunFailure::Program,
        }
    }

    pub(crate) fn details(&self) -> serde_json::Value {
        match self {
            Self::Import(details) => serde_json::to_value(details)
                .expect("the closed import failure detail must serialize"),
            Self::EntryUnreadable | Self::PackageMaterialization | Self::Program => {
                serde_json::Value::Null
            }
        }
    }
}

pub(super) struct ImportTypecheck {
    pub(super) diagnostics: Vec<harn_parser::TypeDiagnostic>,
    pub(super) failure: Option<ImportLoadFailure>,
}

/// Result of [`compile_or_load_chunk_for_run`]. Failures propagate as
/// diagnostic text on the run path plus a [`ChunkLoadFailure`] the caller
/// turns into an exit status.
pub(crate) struct LoadedChunk {
    pub(crate) source: String,
    pub(crate) chunk: harn_vm::Chunk,
    /// The import graph's link table, when the cache lookup proved one current.
    /// Handing it to the VM lets module loading resolve each module's artifact
    /// from a recorded digest instead of re-reading the graph to rederive one.
    pub(crate) link_table: Option<Arc<harn_vm::context_manifest::GraphLinkTable>>,
}

/// Load the entry pipeline as a runnable [`harn_vm::Chunk`], using the
/// content-addressed bytecode cache when its key matches. On a cache miss
/// we read, parse, type-check, and compile, then persist the chunk.
/// On a hit we skip parse/typecheck/compile entirely — the cache invariant
/// is that a stored chunk passed those phases on the writer's harn build,
/// and the key includes every transitively-imported user file so any
/// change re-runs the full path.
///
/// `stderr` receives any diagnostic output. Returns the failure's
/// classification when execution is blocked; the caller maps that to an exit
/// status.
pub(crate) fn compile_or_load_chunk_for_run(
    path: &str,
    stderr: &mut String,
) -> Result<LoadedChunk, ChunkLoadFailure> {
    compile_or_load_chunk_with_timing(path, stderr, None, ProjectContextMode::Project)
}

/// Like [`compile_or_load_chunk_for_run`] but lets the caller observe
/// per-phase wall-clock timings (parse, typecheck, bytecode compile +
/// cache hit/miss). Used by `harn time run` to drive the same code
/// path as `harn run` while reporting phase-level timing.
//
// The `as_deref_mut` calls reborrow the inner `&mut RunTiming` so each
// phase can mutate it independently. Clippy's `needless_option_as_deref`
// is correct that the surface types match — that's exactly the
// reborrow we want.
#[allow(clippy::needless_option_as_deref)]
pub(crate) fn compile_or_load_chunk_with_timing(
    path: &str,
    stderr: &mut String,
    mut timing: Option<&mut RunTiming>,
    project_context: ProjectContextMode,
) -> Result<LoadedChunk, ChunkLoadFailure> {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            stderr.push_str(&format!("Error reading {path}: {e}\n"));
            return Err(ChunkLoadFailure::EntryUnreadable);
        }
    };
    if let Some(t) = timing.as_deref_mut() {
        t.input_bytes = source.len() as u64;
    }

    let compile_phase_start = Instant::now();
    // Project cache entries incorporate manifest-derived compilation
    // authority. Standalone mode cannot safely consume or populate that cache:
    // the same source path may otherwise reuse a privileged project chunk.
    let mut lookup = (project_context == ProjectContextMode::Project)
        .then(|| harn_vm::bytecode_cache::load(Path::new(path), &source));
    if let Some(candidate) = lookup
        .as_mut()
        .filter(|candidate| candidate.chunk.is_some())
    {
        let cache_is_authorized = match candidate.manifest.as_ref() {
            Some(manifest) => package::ensure_cached_dependencies_materialized(
                Path::new(path),
                manifest.package_import_aliases(),
                manifest.package_lock_digest.as_deref(),
            ),
            None => Ok(false),
        };
        match cache_is_authorized {
            Ok(true) => {}
            Ok(false) => {
                candidate.chunk = None;
                candidate.link_table = None;
            }
            Err(error) => {
                stderr.push_str(&format!("error: {error}\n"));
                return Err(ChunkLoadFailure::PackageMaterialization);
            }
        }
    }
    if let Some(chunk) = lookup.as_mut().and_then(|lookup| lookup.chunk.take()) {
        if let Some(t) = timing.as_deref_mut() {
            t.cache_hit = true;
            t.bytecode_compile = compile_phase_start.elapsed();
        }
        return Ok(LoadedChunk {
            source,
            chunk,
            link_table: lookup.and_then(|lookup| lookup.link_table),
        });
    }
    if let Some(t) = timing.as_deref_mut() {
        t.cache_hit = false;
    }

    let parse_start = Instant::now();
    let Some(program) = parse_source_for_run(path, &source, stderr) else {
        return Err(ChunkLoadFailure::Program);
    };
    if let Some(t) = timing.as_deref_mut() {
        t.parse = parse_start.elapsed();
    }

    let typecheck_start = Instant::now();
    let mut had_type_error = false;
    // Materializing the import graph's packages happens inside the type check,
    // so a dependency that could not be prepared surfaces here rather than as a
    // diagnostic about the program's own text.
    let typecheck =
        match typecheck_with_imports(&program, Path::new(path), &source, project_context) {
            Ok(typecheck) => typecheck,
            Err(error) => {
                stderr.push_str(&format!("error: {error}\n"));
                return Err(ChunkLoadFailure::PackageMaterialization);
            }
        };
    for diag in &typecheck.diagnostics {
        let rendered = harn_parser::diagnostic::render_type_diagnostic(&source, path, diag);
        if matches!(diag.severity, DiagnosticSeverity::Error) {
            had_type_error = true;
        }
        stderr.push_str(&rendered);
    }
    if let Some(failure) = typecheck.failure {
        stderr.push_str(&failure.rendered);
        return Err(ChunkLoadFailure::Import(failure.detail));
    }
    if let Some(t) = timing.as_deref_mut() {
        t.typecheck = typecheck_start.elapsed();
    }
    if had_type_error {
        return Err(ChunkLoadFailure::Program);
    }

    let compile_step_start = Instant::now();
    let compiler = match project_context {
        ProjectContextMode::Project => crate::compiler_for_source(Path::new(path), &source),
        ProjectContextMode::Standalone => {
            crate::compiler_for_standalone_source(Path::new(path), &source)
        }
    };
    let chunk = match compiler.compile(&program) {
        Ok(c) => c,
        Err(e) => {
            stderr.push_str(&format!("error: compile error: {e}\n"));
            return Err(ChunkLoadFailure::Program);
        }
    };

    // Cache misses are best-effort — read-only homedirs, full disks, and
    // sandboxes are common in CI environments. Surface the failure as a
    // single-line warning when explicitly requested via the audit hook;
    // otherwise stay quiet to avoid bloating happy-path output.
    if project_context == ProjectContextMode::Project {
        let mut store = harn_vm::bytecode_cache::prepare_entry_store(Path::new(path), &source);
        if let Some(manifest) = store.manifest.as_mut() {
            match package::declared_package_import_aliases(
                Path::new(path),
                manifest.package_import_aliases(),
            ) {
                Ok(aliases) => manifest.package_import_aliases = aliases,
                Err(error) => {
                    stderr.push_str(&format!("error: {error}\n"));
                    return Err(ChunkLoadFailure::PackageMaterialization);
                }
            }
            match package::reachable_dependency_lock_digest(
                Path::new(path),
                manifest.package_import_aliases(),
            ) {
                Ok(digest) => manifest.package_lock_digest = digest,
                Err(error) => {
                    stderr.push_str(&format!("error: {error}\n"));
                    return Err(ChunkLoadFailure::PackageMaterialization);
                }
            }
        }
        if let Err(err) = store.store(&chunk) {
            if std::env::var_os(crate::dispatch::CACHE_DEBUG_ENV).is_some() {
                eprintln!("[harn] bytecode cache write skipped: {err}");
            }
        }
    }
    if let Some(t) = timing.as_deref_mut() {
        t.bytecode_compile = compile_step_start.elapsed();
    }

    Ok(LoadedChunk {
        source,
        chunk,
        link_table: lookup.and_then(|lookup| lookup.link_table),
    })
}

pub(super) fn parse_source_for_run(
    path: &str,
    source: &str,
    stderr: &mut String,
) -> Option<Vec<harn_parser::SNode>> {
    crate::ensure_builtin_signatures_installed();

    let mut lexer = harn_lexer::Lexer::new(source);
    let tokens = match lexer.tokenize() {
        Ok(tokens) => tokens,
        Err(error) => {
            let diagnostic = harn_parser::diagnostic::render_diagnostic_with_code(
                source,
                path,
                &error_span_from_lex(&error),
                "error",
                harn_parser::diagnostic::lexer_error_code(&error),
                &error.to_string(),
                Some("here"),
                None,
            );
            stderr.push_str(&diagnostic);
            return None;
        }
    };

    let mut parser = harn_parser::Parser::new(tokens);
    match parser.parse() {
        Ok(program) => Some(program),
        Err(error) => {
            if parser.all_errors().is_empty() {
                render_parse_error(path, source, &error, stderr);
            } else {
                for error in parser.all_errors() {
                    render_parse_error(path, source, error, stderr);
                }
            }
            None
        }
    }
}

fn render_parse_error(
    path: &str,
    source: &str,
    error: &harn_parser::ParserError,
    stderr: &mut String,
) {
    let span = error_span_from_parse(error);
    let diagnostic = harn_parser::diagnostic::render_diagnostic_with_code(
        source,
        path,
        &span,
        "error",
        harn_parser::diagnostic::parser_error_code(error),
        &harn_parser::diagnostic::parser_error_message(error),
        Some(harn_parser::diagnostic::parser_error_label(error)),
        harn_parser::diagnostic::parser_error_help(error),
    );
    stderr.push_str(&diagnostic);
}

fn error_span_from_lex(error: &harn_lexer::LexerError) -> harn_lexer::Span {
    match error {
        harn_lexer::LexerError::UnexpectedCharacter(_, span)
        | harn_lexer::LexerError::UnterminatedString(span)
        | harn_lexer::LexerError::IntegerLiteralOutOfRange(_, span)
        | harn_lexer::LexerError::UnterminatedBlockComment(span) => *span,
    }
}

fn error_span_from_parse(error: &harn_parser::ParserError) -> harn_lexer::Span {
    match error {
        harn_parser::ParserError::Unexpected { span, .. } => *span,
        harn_parser::ParserError::UnexpectedEof { span, .. } => *span,
    }
}

/// Run the static type checker against `program` with cross-module
/// import-aware call resolution when the file's imports all resolve. Used
/// by `run_file` and the MCP server entry so `harn run` catches undefined
/// cross-module calls before the VM starts.
///
/// The error type is deliberately narrow: the only way this fails outright is
/// that the entry's locked dependencies could not be materialized. Type
/// diagnostics about the program itself come back in the `Ok` payload, so a
/// caller can classify a failure here as a preparation failure without
/// inspecting the message.
pub(super) fn typecheck_with_imports(
    program: &[harn_parser::SNode],
    path: &Path,
    source: &str,
    project_context: ProjectContextMode,
) -> Result<ImportTypecheck, package::PackageError> {
    let graph = match project_context {
        ProjectContextMode::Project => {
            let mut graph = harn_modules::build(&[path.to_path_buf()]);
            if package::ensure_reachable_dependencies_materialized(path, &graph)? {
                graph = harn_modules::build(&[path.to_path_buf()]);
            }
            graph
        }
        ProjectContextMode::Standalone => harn_modules::build_with_standalone_source(path, source),
    };
    let checker = crate::typecheck_imports::checker_with_resolved_graph(
        harn_parser::TypeChecker::new(),
        path,
        &graph,
    );
    Ok(ImportTypecheck {
        diagnostics: checker.check_with_source(program, source),
        failure: super::import_failure::for_run(program, path, source, &graph),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::time::RunTiming;
    use crate::env_guard::ScopedEnvVar;

    #[test]
    fn cache_hit_revalidates_reachable_package_lock_authority() {
        let _state_guard = crate::tests::common::harn_state_lock::lock_harn_state();
        harn_vm::reset_thread_local_state();
        let project = tempfile::tempdir().expect("temp project");
        let cache = tempfile::tempdir().expect("private cache");
        let _cache_dir = ScopedEnvVar::set(
            harn_vm::bytecode_cache::CACHE_DIR_ENV,
            &cache.path().to_string_lossy(),
        );
        let _cache_enabled = ScopedEnvVar::set(harn_vm::bytecode_cache::CACHE_ENABLED_ENV, "1");
        let dependency = project.path().join("vendor/fixture_dep");
        std::fs::create_dir_all(&dependency).expect("create dependency");
        std::fs::write(
            dependency.join("harn.toml"),
            "[package]\nname = \"fixture_dep\"\n",
        )
        .expect("write dependency manifest");
        std::fs::write(
            dependency.join("value.harn"),
            "pub fn package_value() -> int { return 42 }\n",
        )
        .expect("write dependency source");
        let manifest = project.path().join("harn.toml");
        std::fs::write(
            &manifest,
            r#"
[package]
name = "cache-authority-fixture"

[dependencies]
fixture_dep = { path = "./vendor/fixture_dep" }
"#,
        )
        .expect("write manifest");
        crate::package::install_packages_in(
            &crate::package::PackageWorkspace::from_manifest_dir(project.path()),
            false,
            None,
            false,
        )
        .expect("install dependency");
        let entry = project.path().join("main.harn");
        std::fs::write(
            &entry,
            r#"
import { package_value } from "fixture_dep/value"

pipeline main() { return package_value() }
"#,
        )
        .expect("write entry");
        let path = entry.to_string_lossy();
        let mut stderr = String::new();
        compile_or_load_chunk_with_timing(&path, &mut stderr, None, ProjectContextMode::Project)
            .expect("prime cache");
        assert!(stderr.is_empty(), "stderr:\n{stderr}");

        let mut timing = RunTiming::default();
        compile_or_load_chunk_with_timing(
            &path,
            &mut stderr,
            Some(&mut timing),
            ProjectContextMode::Project,
        )
        .expect("prove cache hit before authority changes");
        assert!(
            timing.cache_hit,
            "negative control must reach cache-hit path"
        );

        let replacement = project.path().join("vendor/replacement");
        std::fs::create_dir_all(&replacement).expect("create replacement dependency");
        std::fs::write(
            replacement.join("harn.toml"),
            "[package]\nname = \"fixture_dep\"\n",
        )
        .expect("write replacement manifest");
        std::fs::write(
            replacement.join("value.harn"),
            "pub fn package_value() -> int { return 99 }\n",
        )
        .expect("write replacement source");
        std::fs::write(
            &manifest,
            r#"
[package]
name = "cache-authority-fixture"

[dependencies]
fixture_dep = { path = "./vendor/replacement" }
"#,
        )
        .expect("change dependency authority without changing entry source");
        stderr.clear();
        let error = match compile_or_load_chunk_with_timing(
            &path,
            &mut stderr,
            None,
            ProjectContextMode::Project,
        ) {
            Err(error) => error,
            Ok(_) => panic!("stale lock authority must reject the cached chunk"),
        };
        assert_eq!(error, ChunkLoadFailure::PackageMaterialization);
        assert!(stderr.contains("harn.lock"), "stderr:\n{stderr}");

        crate::package::install_packages_in(
            &crate::package::PackageWorkspace::from_manifest_dir(project.path()),
            false,
            None,
            false,
        )
        .expect("update lock to replacement dependency");
        stderr.clear();
        let mut replacement_timing = RunTiming::default();
        let replacement_chunk = compile_or_load_chunk_with_timing(
            &path,
            &mut stderr,
            Some(&mut replacement_timing),
            ProjectContextMode::Project,
        )
        .expect("compile against replacement authority");
        assert!(stderr.is_empty(), "stderr:\n{stderr}");
        assert!(
            !replacement_timing.cache_hit,
            "a valid new lock must still invalidate the old cached generation"
        );
        assert!(
            replacement_chunk.link_table.is_none(),
            "rejected package authority must discard the old module link table"
        );
        harn_vm::reset_thread_local_state();
    }

    #[test]
    fn cache_miss_rejects_a_package_import_removed_from_the_manifest() {
        let _state_guard = crate::tests::common::harn_state_lock::lock_harn_state();
        harn_vm::reset_thread_local_state();
        let project = tempfile::tempdir().expect("temp project");
        let cache = tempfile::tempdir().expect("private cache");
        let _cache_enabled = ScopedEnvVar::set(harn_vm::bytecode_cache::CACHE_ENABLED_ENV, "0");
        let dependency = project.path().join("vendor/fixture_dep");
        std::fs::create_dir_all(&dependency).expect("create dependency");
        std::fs::write(
            dependency.join("harn.toml"),
            "[package]\nname = \"fixture_dep\"\n",
        )
        .expect("write dependency manifest");
        std::fs::write(
            dependency.join("value.harn"),
            "pub fn package_value() -> int { return 42 }\n",
        )
        .expect("write dependency source");
        let manifest = project.path().join("harn.toml");
        std::fs::write(
            &manifest,
            r#"
[package]
name = "removed-alias-fixture"

[dependencies]
fixture_dep = { path = "./vendor/fixture_dep" }
"#,
        )
        .expect("write manifest");
        crate::package::install_packages_in(
            &crate::package::PackageWorkspace::for_test(project.path(), cache.path()),
            false,
            None,
            false,
        )
        .expect("install dependency");
        let entry = project.path().join("main.harn");
        std::fs::write(
            &entry,
            "import { package_value } from \"fixture_dep/value\"\n\npipeline main() { return package_value() }\n",
        )
        .expect("write entry");
        std::fs::write(&manifest, "[package]\nname = \"removed-alias-fixture\"\n")
            .expect("remove dependency declaration while retaining its generation");

        let mut stderr = String::new();
        let result = compile_or_load_chunk_with_timing(
            &entry.to_string_lossy(),
            &mut stderr,
            None,
            ProjectContextMode::Project,
        );

        assert!(matches!(
            result,
            Err(ChunkLoadFailure::PackageMaterialization)
        ));
        assert!(stderr.contains("fixture_dep"), "stderr:\n{stderr}");
        assert!(stderr.contains("dependencies"), "stderr:\n{stderr}");
        harn_vm::reset_thread_local_state();
    }
}
