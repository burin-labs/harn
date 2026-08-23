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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChunkLoadFailure {
    /// The entry file could not be read.
    EntryUnreadable,
    /// The run's locked dependencies could not be materialized.
    PackageMaterialization,
    /// The program did not parse, typecheck, or compile.
    Program,
}

impl ChunkLoadFailure {
    /// The `--json` error event's `code`. Program failures keep reporting
    /// `compile_error`; the preparation codes are new because there was
    /// previously nothing to name.
    pub(crate) fn diagnostic_code(self) -> &'static str {
        match self {
            Self::EntryUnreadable => "entry_unreadable",
            Self::PackageMaterialization => "package_materialization",
            Self::Program => "compile_error",
        }
    }

    pub(crate) fn classification(self) -> crate::exit::RunFailure {
        match self {
            Self::EntryUnreadable | Self::PackageMaterialization => crate::exit::RunFailure::Setup,
            Self::Program => crate::exit::RunFailure::Program,
        }
    }
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
    compile_or_load_chunk_with_timing(path, stderr, None)
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
    let lookup = harn_vm::bytecode_cache::load(Path::new(path), &source);
    if let Some(chunk) = lookup.chunk {
        if let Some(t) = timing.as_deref_mut() {
            t.cache_hit = true;
            t.bytecode_compile = compile_phase_start.elapsed();
        }
        return Ok(LoadedChunk {
            source,
            chunk,
            link_table: lookup.link_table,
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
    let type_diagnostics = match typecheck_with_imports(&program, Path::new(path), &source) {
        Ok(diagnostics) => diagnostics,
        Err(error) => {
            stderr.push_str(&format!("error: {error}\n"));
            return Err(ChunkLoadFailure::PackageMaterialization);
        }
    };
    for diag in &type_diagnostics {
        let rendered = harn_parser::diagnostic::render_type_diagnostic(&source, path, diag);
        if matches!(diag.severity, DiagnosticSeverity::Error) {
            had_type_error = true;
        }
        stderr.push_str(&rendered);
    }
    if let Some(t) = timing.as_deref_mut() {
        t.typecheck = typecheck_start.elapsed();
    }
    if had_type_error {
        return Err(ChunkLoadFailure::Program);
    }

    let compile_step_start = Instant::now();
    let chunk = match crate::compiler_for_source(Path::new(path), &source).compile(&program) {
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
    if let Err(err) = lookup.store(&chunk) {
        if std::env::var_os(crate::dispatch::CACHE_DEBUG_ENV).is_some() {
            eprintln!("[harn] bytecode cache write skipped: {err}");
        }
    }
    if let Some(t) = timing.as_deref_mut() {
        t.bytecode_compile = compile_step_start.elapsed();
    }

    Ok(LoadedChunk {
        source,
        chunk,
        link_table: lookup.link_table,
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
) -> Result<Vec<harn_parser::TypeDiagnostic>, package::PackageError> {
    package::ensure_dependencies_materialized(path)?;
    let checker = crate::typecheck_imports::checker_with_resolved_imports(
        harn_parser::TypeChecker::new(),
        path,
    );
    Ok(checker.check_with_source(program, source))
}
