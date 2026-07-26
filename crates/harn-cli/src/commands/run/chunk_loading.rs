//! Loading the entry pipeline as a runnable chunk.
//!
//! One cohesive step: consult the content-addressed bytecode cache, and on a
//! miss parse, type-check, compile and persist. Kept apart from the rest of
//! `harn run` because it is the only part that deals in cache keys, artifacts,
//! and compile diagnostics rather than in process and sandbox setup.

use super::*;

/// Result of [`compile_or_load_chunk_for_run`]. Failures propagate as
/// diagnostic text on the run path so callers map them straight to a
/// non-zero exit code without bespoke error types.
pub(crate) struct LoadedChunk {
    pub(crate) source: String,
    pub(crate) chunk: harn_vm::Chunk,
    /// Link plan for this entry's import graph, present only on a cache hit
    /// whose manifest re-checked clean. Handed to the VM so module loading can
    /// resolve artifacts from recorded digests instead of re-reading the graph.
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
/// `stderr` receives any diagnostic output. Returns `None` when a fatal
/// type or compile error blocks execution; the caller maps that to
/// exit-code 1.
pub(crate) fn compile_or_load_chunk_for_run(
    path: &str,
    stderr: &mut String,
) -> Option<LoadedChunk> {
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
) -> Option<LoadedChunk> {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            stderr.push_str(&format!("Error reading {path}: {e}\n"));
            return None;
        }
    };
    if let Some(t) = timing.as_deref_mut() {
        t.input_bytes = source.len() as u64;
    }

    let compile_phase_start = Instant::now();
    let lookup = harn_vm::bytecode_cache::load(Path::new(path), &source);
    // Derived before the chunk is taken, and only when there is a chunk to
    // serve: resolving a plan re-checks the manifest, which is wasted work on a
    // miss whose chunk still has to be compiled from source anyway.
    let link_table = lookup
        .chunk
        .is_some()
        .then(|| lookup.link_table())
        .flatten();
    if let Some(chunk) = lookup.chunk {
        if let Some(t) = timing.as_deref_mut() {
            t.cache_hit = true;
            t.bytecode_compile = compile_phase_start.elapsed();
        }
        return Some(LoadedChunk {
            source,
            chunk,
            link_table,
        });
    }
    if let Some(t) = timing.as_deref_mut() {
        t.cache_hit = false;
    }

    let parse_start = Instant::now();
    let program = parse_source_for_run(path, &source, stderr)?;
    if let Some(t) = timing.as_deref_mut() {
        t.parse = parse_start.elapsed();
    }

    let typecheck_start = Instant::now();
    let mut had_type_error = false;
    let type_diagnostics = match typecheck_with_imports(&program, Path::new(path), &source) {
        Ok(diagnostics) => diagnostics,
        Err(error) => {
            stderr.push_str(&format!("error: {error}\n"));
            return None;
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
        return None;
    }

    let compile_step_start = Instant::now();
    let chunk = match crate::compiler_for_source(Path::new(path), &source).compile(&program) {
        Ok(c) => c,
        Err(e) => {
            stderr.push_str(&format!("error: compile error: {e}\n"));
            return None;
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

    Some(LoadedChunk {
        source,
        chunk,
        link_table: None,
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
pub(super) fn typecheck_with_imports(
    program: &[harn_parser::SNode],
    path: &Path,
    source: &str,
) -> Result<Vec<harn_parser::TypeDiagnostic>, String> {
    package::ensure_dependencies_materialized(path)?;
    let checker = crate::typecheck_imports::checker_with_resolved_imports(
        harn_parser::TypeChecker::new(),
        path,
    );
    Ok(checker.check_with_source(program, source))
}
