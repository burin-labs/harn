//! A source census includes the entry module's transitive imports. Discovery
//! remains owned by the type checker; syntax only avoids checking modules that
//! cannot contain a predicate operation.

use std::collections::BTreeSet;
use std::path::Path;

use harn_kernel::predicate::PredicateManifest;
use harn_parser::analysis::{AnalysisDatabase, SourceId, SourceVersion};
use harn_parser::{DiagnosticSeverity, Node, PredicateSite};

use crate::package::CheckConfig;

pub(super) fn collect(
    analysis: &mut AnalysisDatabase,
    root: &Path,
    sites: &[PredicateSite],
    config: &CheckConfig,
    graph: &harn_modules::ModuleGraph,
) -> Result<PredicateManifest, String> {
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
                ));
            };
            if !seen.insert(path.clone()) {
                continue;
            }
            pending.push(path.clone());
            let source = harn_modules::read_module_source(&path)
                .ok_or_else(|| format!("cannot read predicate census source {}", path.display()))?;
            let id = SourceId::path(&path);
            analysis.set_source(id.clone(), source, SourceVersion(1));
            let parsed = analysis.parse(&id).map_err(|error| {
                format!(
                    "cannot parse predicate census source {}: {error:?}",
                    path.display()
                )
            })?;
            let mut candidate = false;
            harn_parser::visit::walk_program_interpolated(
                &parsed.source,
                &parsed.program,
                &mut |node| {
                    let method = match &node.node {
                        Node::MethodCall { method, .. }
                        | Node::OptionalMethodCall { method, .. } => method,
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
                    });
                },
            );
            if !candidate {
                continue;
            }
            let checked = analysis
                .typecheck(&id, super::analysis::typecheck_config(&path, config, graph))
                .map_err(|error| {
                    format!(
                        "cannot check predicate census source {}: {error:?}",
                        path.display()
                    )
                })?;
            let errors: Vec<_> = checked
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
                .map(|diagnostic| diagnostic.message.as_str())
                .collect();
            if !errors.is_empty() {
                return Err(format!(
                    "predicate census source {} failed checking: {}",
                    path.display(),
                    errors.join("; ")
                ));
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
