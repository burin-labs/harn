//! A source census includes the entry module's transitive imports. Discovery
//! remains owned by the type checker; syntax only avoids checking modules that
//! cannot contain a predicate operation.

use std::collections::BTreeSet;
use std::path::Path;

use harn_kernel::predicate::PredicateManifest;
use harn_parser::analysis::{AnalysisDatabase, SourceId, SourceVersion};
use harn_parser::{DiagnosticSeverity, Node, PredicateSite};

use crate::package::CheckConfig;

pub(super) struct CensusError {
    pub message: String,
    pub code: Option<String>,
}

impl From<String> for CensusError {
    fn from(message: String) -> Self {
        Self {
            message,
            code: None,
        }
    }
}

pub(super) fn collect(
    analysis: &mut AnalysisDatabase,
    root: &Path,
    sites: &[PredicateSite],
    config: &CheckConfig,
    graph: &harn_modules::ModuleGraph,
) -> Result<PredicateManifest, CensusError> {
    let mut manifest = PredicateManifest::from_checked_sites(&root.to_string_lossy(), sites);
    let mut seen = BTreeSet::from([root.canonicalize().unwrap_or_else(|_| root.into())]);
    let mut pending = vec![root.to_path_buf()];
    while let Some(parent) = pending.pop() {
        for import in graph.imports_for_module(&parent) {
            let Some(path) = import.resolved_path else {
                return Err(format!(
                    "cannot census unresolved import '{}' in {}",
                    import.raw_path,
                    parent.display()
                )
                .into());
            };
            if !seen.insert(path.clone()) {
                continue;
            }
            pending.push(path.clone());
            let parsed = census_parse(analysis, &path)?;
            let (source, program) = &*parsed;
            let mut candidate = false;
            harn_parser::visit::walk_program_interpolated(source, program, &mut |node| {
                let method = match &node.node {
                    Node::MethodCall { method, .. } | Node::OptionalMethodCall { method, .. } => {
                        method
                    }
                    Node::PropertyAccess { property, .. }
                    | Node::OptionalPropertyAccess { property, .. } => property,
                    _ => return,
                };
                candidate |= harn_parser::builtin_signatures::lookup_capability_method(
                    harn_builtin_meta::CapabilityId::Llm,
                    method,
                )
                .is_some_and(|signature| {
                    signature.name == harn_builtin_meta::predicate::EVALUATE.name
                        || signature.name == harn_builtin_meta::predicate::EVALUATE_REQUEST.name
                        || signature.name == harn_builtin_meta::predicate::EVALUATE_PREDICATE.name
                });
            });
            if !candidate {
                continue;
            }
            let id = SourceId::path(&path);
            analysis.set_source(id.clone(), source.clone(), SourceVersion(1));
            let mut checked = analysis
                .typecheck(&id, super::analysis::typecheck_config(&path, config, graph))
                .map_err(|error| {
                    format!(
                        "cannot check predicate census source {}: {error:?}",
                        path.display()
                    )
                })?;
            checked
                .diagnostics
                .extend(harn_vm::provider_catalog::validate_predicate_models(
                    &checked.predicate_sites,
                ));
            let errors: Vec<_> = checked
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
                .collect();
            if !errors.is_empty() {
                return Err(CensusError {
                    message: format!(
                        "predicate census source {} failed checking: {}",
                        path.display(),
                        errors
                            .iter()
                            .map(|error| error.message.as_str())
                            .collect::<Vec<_>>()
                            .join("; ")
                    ),
                    code: Some(errors[0].code.to_string()),
                });
            }
            manifest.sites.extend(
                PredicateManifest::from_checked_sites(
                    &path.to_string_lossy(),
                    &checked.predicate_sites,
                )
                .sites,
            );
        }
    }
    manifest.sites.sort_by(|a, b| {
        (&a.source, a.line, a.column, &a.id).cmp(&(&b.source, b.line, b.column, &b.id))
    });
    Ok(manifest)
}

/// Parse one census module through the process-wide memo.
///
/// Every module in the import closure is scanned, but only the few that can
/// contain a predicate operation are type checked. Putting every scanned
/// module into the worker's analysis database kept a private copy of the
/// whole closure's source, tokens, and AST alive in each worker, so a cold
/// `harn check` peaked at about workers x closure size (harn#8899). The memo
/// holds one shared parse per module for the process.
///
/// The memo keeps no error detail, so a module it cannot parse is re-read
/// through the analysis database to report the same error as before.
fn census_parse(
    analysis: &mut AnalysisDatabase,
    path: &Path,
) -> Result<super::source::ParsedModule, CensusError> {
    if let Some(parsed) = super::source::parse_resolved_module(path) {
        return Ok(parsed);
    }
    let source = harn_modules::read_module_source(path)
        .ok_or_else(|| format!("cannot read predicate census source {}", path.display()))?;
    let id = SourceId::path(path);
    analysis.set_source(id.clone(), source, SourceVersion(1));
    let parsed = analysis.parse(&id);
    analysis.remove_source(&id);
    let parsed = parsed.map_err(|error| {
        format!(
            "cannot parse predicate census source {}: {error:?}",
            path.display()
        )
    })?;
    Ok(std::sync::Arc::new((parsed.source, parsed.program)))
}
