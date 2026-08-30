//! The typed run-launch projection for import failures.
//!
//! The module graph owns import resolution and classification. This module
//! turns its first blocking fact into one portable machine contract plus the
//! matching human diagnostic. The chunk loader and JSON emitter consume that
//! same value, so adding another run surface cannot silently reclassify it.

use std::path::Path;

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum FailureDetailKind {
    ImportFailure,
}

/// Machine-readable import facts retained at the run-launch boundary.
///
/// `source` is workspace-relative when the target belongs to the project. A
/// module URI is used for targets outside that root, so this contract never
/// leaks a host-specific absolute path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ImportFailureDetail {
    kind: FailureDetailKind,
    failure_class: ImportFailureClass,
    module: String,
    symbol: Option<String>,
    source: String,
    harn_version: &'static str,
    harn_revision: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ImportFailureClass {
    UnresolvedModule,
    MissingImportedSymbol,
    PrivateImportedSymbol,
    ImportedModuleCompileFailure,
}

pub(super) struct ImportLoadFailure {
    pub(super) detail: ImportFailureDetail,
    pub(super) rendered: String,
}

pub(super) fn for_run(
    program: &[harn_parser::SNode],
    entry_path: &Path,
    source: &str,
    graph: &harn_modules::ModuleGraph,
) -> Option<ImportLoadFailure> {
    if let Some(failure) = graph.import_compile_failures(entry_path).into_iter().next() {
        let message = format!(
            "imported module '{}' failed to compile ({}): {}",
            failure.import_raw_path,
            failure.module_path.display(),
            failure.error.message,
        );
        let help = format!(
            "fix the lex/parse error in {} before this import can resolve",
            failure.module_path.display(),
        );
        return Some(ImportLoadFailure {
            detail: detail(
                entry_path,
                ImportFailureClass::ImportedModuleCompileFailure,
                &failure.import_raw_path,
                None,
                Some(&failure.module_path),
            ),
            rendered: render(
                source,
                entry_path,
                &failure.import_span,
                harn_parser::diagnostic_codes::Code::ModuleImportCompileFailed,
                &message,
                Some(&help),
            ),
        });
    }

    if let Some(issue) = graph.selective_import_issues(entry_path).into_iter().next() {
        let failure_class = match issue.kind {
            harn_modules::SelectiveImportIssueKind::Missing => {
                ImportFailureClass::MissingImportedSymbol
            }
            harn_modules::SelectiveImportIssueKind::Private => {
                ImportFailureClass::PrivateImportedSymbol
            }
        };
        let resolved = graph
            .imports_for_module(entry_path)
            .into_iter()
            .find(|import| import.raw_path == issue.module)
            .and_then(|import| import.resolved_path);
        return Some(ImportLoadFailure {
            detail: detail(
                entry_path,
                failure_class,
                &issue.module,
                Some(issue.name.clone()),
                resolved.as_deref(),
            ),
            rendered: render(
                source,
                entry_path,
                &issue.span,
                harn_parser::diagnostic_codes::Code::ImportSymbolMissing,
                &issue.message(),
                Some(&issue.help()),
            ),
        });
    }

    let unresolved = graph
        .imports_for_module(entry_path)
        .into_iter()
        .find(|import| import.resolved_path.is_none())?;
    let span = import_span(program, &unresolved.raw_path)
        .unwrap_or_else(|| harn_lexer::Span::with_offsets(0, 0, 1, 1));
    let message = format!("unresolved import '{}'", unresolved.raw_path);
    Some(ImportLoadFailure {
        detail: detail(
            entry_path,
            ImportFailureClass::UnresolvedModule,
            &unresolved.raw_path,
            unresolved
                .selective_names
                .as_ref()
                .and_then(|names| (names.len() == 1).then(|| names[0].clone())),
            None,
        ),
        rendered: render(
            source,
            entry_path,
            &span,
            harn_parser::diagnostic_codes::Code::ModuleImportUnresolved,
            &message,
            Some("create the module or correct the import specifier"),
        ),
    })
}

fn detail(
    entry_path: &Path,
    failure_class: ImportFailureClass,
    module: &str,
    symbol: Option<String>,
    resolved_source: Option<&Path>,
) -> ImportFailureDetail {
    ImportFailureDetail {
        kind: FailureDetailKind::ImportFailure,
        failure_class,
        module: module.to_string(),
        symbol,
        source: normalized_source_identity(entry_path, resolved_source, module),
        harn_version: env!("CARGO_PKG_VERSION"),
        harn_revision: nonempty_build_revision(),
    }
}

fn nonempty_build_revision() -> Option<&'static str> {
    let revision = env!("HARN_BUILD_REVISION");
    (!revision.is_empty()).then_some(revision)
}

fn normalized_source_identity(
    entry_path: &Path,
    resolved_source: Option<&Path>,
    module: &str,
) -> String {
    let source = resolved_source.unwrap_or(entry_path);
    if let Some(stdlib) = source.to_str().and_then(|path| path.strip_prefix("<std>/")) {
        return format!("harn://std/{stdlib}");
    }
    let entry = harn_modules::canonical_path(entry_path);
    let root = harn_modules::manifest_walk::find_project_root(&entry)
        .unwrap_or_else(|| entry.parent().unwrap_or(Path::new(".")).to_path_buf());
    let root = harn_modules::canonical_path(&root);
    let source = harn_modules::canonical_path(source);
    if let Ok(relative) = source.strip_prefix(&root) {
        let relative = normalize_wire_path(relative);
        if !relative.is_empty() {
            return relative;
        }
    }
    format!("harn://module/{}", module.trim_start_matches("./"))
}

fn normalize_wire_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => Some(value.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn import_span(program: &[harn_parser::SNode], raw_path: &str) -> Option<harn_lexer::Span> {
    program.iter().find_map(|node| match &node.node {
        harn_parser::Node::ImportDecl { path, .. }
        | harn_parser::Node::SelectiveImport { path, .. }
        | harn_parser::Node::NamespaceImport { path, .. }
            if path == raw_path =>
        {
            Some(node.span)
        }
        _ => None,
    })
}

fn render(
    source: &str,
    entry_path: &Path,
    span: &harn_lexer::Span,
    code: harn_parser::diagnostic_codes::Code,
    message: &str,
    help: Option<&str>,
) -> String {
    harn_parser::diagnostic::render_diagnostic_with_code(
        source,
        &entry_path.to_string_lossy(),
        span,
        "error",
        code,
        message,
        Some("import fails here"),
        help,
    )
}
