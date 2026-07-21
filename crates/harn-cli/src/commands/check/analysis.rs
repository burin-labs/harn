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
    let namespace_imports = module_graph
        .namespace_imports_for_file(path)
        .unwrap_or_default()
        .into_iter()
        .map(|info| {
            (
                info.alias,
                harn_parser::NamespaceImportBinding {
                    // Prefer the import string as written (`std/text`,
                    // `./lib`) so diagnostics stay stable across machines.
                    module_path: info.raw_path,
                    members: info.member_names.into_iter().collect(),
                },
            )
        })
        .collect();
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
        .with_namespace_imports(namespace_imports)
}

pub(crate) fn render_file_analysis_error_or_exit(path: &str, error: FileAnalysisError) -> ! {
    eprint!("{}", render_file_analysis_error_to_string(path, &error));
    std::process::exit(1);
}

/// Render a lex/parse/read failure exactly as the exiting variant prints it,
/// but into a string, so buffered drivers can replay it in file order and
/// keep checking the remaining files.
pub(crate) fn render_file_analysis_error_to_string(
    path: &str,
    error: &FileAnalysisError,
) -> String {
    match error {
        FileAnalysisError::Read(error) => {
            format!("Error reading {path}: {error}\n")
        }
        FileAnalysisError::Analysis(AnalysisError::Lex { source, error }) => {
            harn_parser::diagnostic::render_diagnostic_with_code(
                source,
                path,
                &span_from_lexer_error(error),
                "error",
                harn_parser::diagnostic::lexer_error_code(error),
                &error.to_string(),
                Some("here"),
                None,
            )
        }
        FileAnalysisError::Analysis(AnalysisError::Parse { source, errors }) => {
            let mut out = String::new();
            for error in errors {
                out.push_str(&harn_parser::diagnostic::render_diagnostic_with_code(
                    source,
                    path,
                    &span_from_parser_error(error),
                    "error",
                    harn_parser::diagnostic::parser_error_code(error),
                    &harn_parser::diagnostic::parser_error_message(error),
                    Some(harn_parser::diagnostic::parser_error_label(error)),
                    harn_parser::diagnostic::parser_error_help(error),
                ));
            }
            out
        }
        FileAnalysisError::Analysis(AnalysisError::MissingSource(id)) => {
            format!("missing analysis source {}\n", id.as_str())
        }
    }
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
