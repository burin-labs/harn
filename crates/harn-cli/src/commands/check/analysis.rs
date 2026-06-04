use std::path::Path;

use harn_parser::analysis::{
    AnalysisDatabase, AnalysisError, SourceId, SourceVersion, TypeCheckConfig, TypeCheckOutput,
};

use crate::package::CheckConfig;

#[derive(Debug)]
pub(crate) enum FileAnalysisError {
    Read(std::io::Error),
    Analysis(AnalysisError),
}

pub(crate) fn analyze_file(
    analysis: &mut AnalysisDatabase,
    path: &Path,
    config: &CheckConfig,
    module_graph: &harn_modules::ModuleGraph,
) -> Result<TypeCheckOutput, FileAnalysisError> {
    crate::ensure_builtin_signatures_installed();

    let source = std::fs::read_to_string(path).map_err(FileAnalysisError::Read)?;
    let id = SourceId::path(path);
    analysis.set_source(id.clone(), source, SourceVersion(1));
    analysis
        .typecheck(&id, typecheck_config(path, config, module_graph))
        .map_err(FileAnalysisError::Analysis)
}

pub(crate) fn typecheck_config(
    path: &Path,
    config: &CheckConfig,
    module_graph: &harn_modules::ModuleGraph,
) -> TypeCheckConfig {
    TypeCheckConfig::new()
        .with_strict_types(config.strict_types)
        .with_imported_names(module_graph.imported_names_for_file(path))
        .with_imported_type_decls(
            module_graph
                .imported_type_declarations_for_file(path)
                .unwrap_or_default(),
        )
        .with_imported_callable_decls(
            module_graph
                .imported_callable_declarations_for_file(path)
                .unwrap_or_default(),
        )
}

pub(crate) fn render_file_analysis_error_or_exit(path: &str, error: FileAnalysisError) -> ! {
    match error {
        FileAnalysisError::Read(error) => {
            eprintln!("Error reading {path}: {error}");
        }
        FileAnalysisError::Analysis(AnalysisError::Lex { source, error }) => {
            let diagnostic = harn_parser::diagnostic::render_diagnostic_with_code(
                &source,
                path,
                &span_from_lexer_error(&error),
                "error",
                harn_parser::diagnostic::lexer_error_code(&error),
                &error.to_string(),
                Some("here"),
                None,
            );
            eprint!("{diagnostic}");
        }
        FileAnalysisError::Analysis(AnalysisError::Parse { source, errors }) => {
            for error in &errors {
                let diagnostic = harn_parser::diagnostic::render_diagnostic_with_code(
                    &source,
                    path,
                    &span_from_parser_error(error),
                    "error",
                    harn_parser::diagnostic::parser_error_code(error),
                    &harn_parser::diagnostic::parser_error_message(error),
                    Some(harn_parser::diagnostic::parser_error_label(error)),
                    harn_parser::diagnostic::parser_error_help(error),
                );
                eprint!("{diagnostic}");
            }
        }
        FileAnalysisError::Analysis(AnalysisError::MissingSource(id)) => {
            eprintln!("missing analysis source {}", id.as_str());
        }
    }
    std::process::exit(1);
}

pub(crate) fn span_from_lexer_error(error: &harn_lexer::LexerError) -> harn_lexer::Span {
    match error {
        harn_lexer::LexerError::UnexpectedCharacter(_, span)
        | harn_lexer::LexerError::UnterminatedString(span)
        | harn_lexer::LexerError::IntegerLiteralOutOfRange(_, span)
        | harn_lexer::LexerError::UnterminatedBlockComment(span) => *span,
    }
}

pub(crate) fn span_from_parser_error(error: &harn_parser::ParserError) -> harn_lexer::Span {
    match error {
        harn_parser::ParserError::Unexpected { span, .. }
        | harn_parser::ParserError::UnexpectedEof { span, .. } => *span,
    }
}
