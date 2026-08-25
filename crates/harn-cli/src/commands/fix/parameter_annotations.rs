//! `harn fix` repair: give every declared parameter a type.
//!
//! `HARN-TYP-028` says a parameter has no type. This pass says what the type
//! is. It reads the whole module graph once, infers each parameter from the
//! body that uses it and the call sites that feed it, and then re-checks the
//! file it is about to rewrite. An inferred type that makes the file stop
//! checking is thrown away and the parameter falls back to `unknown`, so the
//! migration can never trade a missing annotation for a broken build.
//!
//! `unknown` is a real outcome, not a failure mode to hide. The pass counts every
//! parameter it gave up on and why, and reports the count with the plan.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use harn_lexer::{FixEdit, Span};
use harn_parser::analysis::{AnalysisDatabase, SourceId, SourceVersion};
use harn_parser::param_annotations::{self, UnannotatedParam};
use harn_parser::{DiagnosticCode as Code, Repair, RepairId, RepairSafety, SNode};

use super::{RepairCandidate, RepairImpactWire};
use crate::commands;
use crate::package;

#[path = "parameter_annotations/inference.rs"]
mod inference;

use inference::{Cause, Inference, ModuleFacts, SettledTypes};

/// How the migration went, in numbers a human can act on.
#[derive(Debug, Default, Clone)]
pub(super) struct AnnotationResidue {
    /// Parameters that got a type the program proved.
    pub(super) inferred: usize,
    /// Parameters left as validated `unknown` boundaries for a human to review.
    pub(super) unresolved: usize,
    /// Count per reason, across both outcomes.
    pub(super) causes: BTreeMap<&'static str, usize>,
}

impl AnnotationResidue {
    fn record(&mut self, inference: &Inference) {
        if inference.rendered != "unknown" && inference.rendered != "any" {
            self.inferred += 1;
        } else {
            self.unresolved += 1;
        }
        *self.causes.entry(inference.cause.as_str()).or_default() += 1;
    }

    pub(super) fn total(&self) -> usize {
        self.inferred + self.unresolved
    }

    /// Share of parameters the pass could not prove anything about.
    pub(super) fn unresolved_share(&self) -> f64 {
        let total = self.total();
        if total == 0 {
            return 0.0;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "counts are parameter tallies, far below f64's exact integer range"
        )]
        {
            self.unresolved as f64 / total as f64
        }
    }
}

struct Module {
    path: PathBuf,
    source: String,
    program: Vec<SNode>,
    sites: Vec<UnannotatedParam>,
}

/// Plan an annotation for every parameter `HARN-TYP-028` reports.
pub(super) fn plan(
    files: &[PathBuf],
    module_graph: &harn_modules::ModuleGraph,
    analysis: &mut AnalysisDatabase,
) -> (Vec<RepairCandidate>, AnnotationResidue) {
    let mut modules: Vec<Module> = Vec::new();
    for file in files {
        let config = check_config(file);
        let output = commands::check::analyze_file(analysis, file, &config, module_graph);
        analysis.remove_source(&SourceId::path(file));
        let Ok(output) = output else {
            continue;
        };
        modules.push(Module {
            path: file.clone(),
            sites: param_annotations::unannotated_params(&output.program),
            source: output.source,
            program: output.program,
        });
    }

    let inferences = converge(&modules, module_graph);

    let mut candidates = Vec::new();
    let mut residue = AnnotationResidue::default();
    for (module, mut chosen) in modules.iter().zip(inferences) {
        if module.sites.is_empty() {
            continue;
        }
        settle(module, module_graph, analysis, &mut chosen);
        for (site, inference) in module.sites.iter().zip(&chosen) {
            residue.record(inference);
            let Some(edit) = annotation_edit(&module.source, site, &inference.rendered) else {
                continue;
            };
            candidates.push(candidate(
                &module.path,
                site,
                &inference.rendered,
                inference.cause,
                edit,
            ));
        }
    }
    (candidates, residue)
}

/// Infer every parameter, then infer again with the answers in hand.
///
/// One pass can only use the types already written in the source, so a helper
/// whose caller is itself unannotated stays unknown. Feeding each round's
/// inferences back as declared types lets the answer propagate along the call
/// graph until it stops changing.
fn converge(modules: &[Module], module_graph: &harn_modules::ModuleGraph) -> Vec<Vec<Inference>> {
    const MAX_ROUNDS: usize = 6;
    let resolutions = module_resolutions(modules, module_graph);
    let mut settled = SettledTypes::new();
    let mut chosen: Vec<Vec<Inference>> = Vec::new();
    for _ in 0..MAX_ROUNDS {
        let mut facts = ModuleFacts::default();
        for (index, module) in modules.iter().enumerate() {
            facts.merge(inference::collect(
                index,
                &module.source,
                &module.program,
                &settled,
                &resolutions[index],
            ));
        }
        chosen = modules
            .iter()
            .enumerate()
            .map(|(index, module)| {
                module
                    .sites
                    .iter()
                    .map(|site| inference::infer(index, site, &facts))
                    .collect()
            })
            .collect();
        let next = settled_types(modules, &chosen);
        if next == settled {
            break;
        }
        settled = next;
    }
    chosen
}

fn settled_types(modules: &[Module], chosen: &[Vec<Inference>]) -> SettledTypes {
    let mut settled = SettledTypes::new();
    for (module_index, (module, inferences)) in modules.iter().zip(chosen).enumerate() {
        for (site, inference) in module.sites.iter().zip(inferences) {
            if inference.cause.is_inferred() && inference.rendered != "unknown" {
                settled.insert(
                    (module_index, site.owner.clone(), site.index),
                    inference.rendered.clone(),
                );
            }
        }
    }
    settled
}

fn module_resolutions(
    modules: &[Module],
    module_graph: &harn_modules::ModuleGraph,
) -> Vec<inference::ModuleResolution> {
    let mut indexes = std::collections::HashMap::new();
    for (index, module) in modules.iter().enumerate() {
        for key in module_path_keys(&module.path) {
            indexes.insert(key, index);
        }
    }

    modules
        .iter()
        .enumerate()
        .map(|(index, module)| {
            let mut resolution = inference::ModuleResolution::default();
            let mut local = std::collections::HashSet::new();
            harn_parser::visit::walk_program(&module.program, &mut |node| {
                let name = match &node.node {
                    harn_parser::Node::FnDecl { name, .. }
                    | harn_parser::Node::Pipeline { name, .. }
                    | harn_parser::Node::ToolDecl { name, .. } => Some(name),
                    _ => None,
                };
                if let Some(name) = name {
                    local.insert(name.clone());
                    resolution.callables.insert(name.clone(), index);
                }
            });

            let mut ambiguous = std::collections::HashSet::new();
            for import in module_graph.imports_for_module(&module.path) {
                let Some(path) = import.resolved_path.as_ref() else {
                    continue;
                };
                let Some(target) = module_path_keys(path)
                    .into_iter()
                    .find_map(|key| indexes.get(&key).copied())
                else {
                    continue;
                };
                if let Some(alias) = import.namespace_alias {
                    resolution.namespaces.insert(alias, target);
                    continue;
                }
                let names = import
                    .selective_names
                    .unwrap_or_else(|| module_graph.exports_for_module(path));
                for name in names {
                    if local.contains(&name) {
                        continue;
                    }
                    match resolution.callables.insert(name.clone(), target) {
                        Some(previous) if previous != target => {
                            ambiguous.insert(name);
                        }
                        _ => {}
                    }
                }
            }
            for name in ambiguous {
                resolution.callables.remove(&name);
            }
            resolution
        })
        .collect()
}

fn module_path_keys(path: &Path) -> Vec<PathBuf> {
    let mut keys = vec![std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())];
    let marker = "crates/harn-stdlib/src/stdlib/";
    if let Some((_, relative)) = path.to_string_lossy().split_once(marker) {
        keys.push(Path::new("<std>").join(relative).with_extension(""));
    }
    keys
}

fn check_config(file: &Path) -> package::CheckConfig {
    let mut config = package::load_check_config(Some(file));
    commands::check::apply_harn_lint_config(file, &mut config);
    config
}

/// Keep the largest deterministic subset of inferred annotations that checks.
///
/// Every site outside the subset is written as explicit `unknown`, preserving
/// a dynamic input boundary while requiring safe narrowing before use. A clean
/// chunk can be accepted at once. A failing chunk is split until the one
/// annotation that introduces a new error is isolated. Accepted chunks stay
/// enabled while the next chunk is tested, so the final set is proven together
/// rather than only one annotation at a time.
fn settle(
    module: &Module,
    module_graph: &harn_modules::ModuleGraph,
    analysis: &mut AnalysisDatabase,
    chosen: &mut [Inference],
) {
    let sites = module.sites.as_slice();
    let config = check_config(&module.path);
    let baseline = check_diagnostics(module, &module.source, module_graph, analysis, &config, 0);
    let inferred = chosen
        .iter()
        .enumerate()
        .filter_map(|(index, inference)| inference.cause.is_inferred().then_some(index))
        .collect::<Vec<_>>();
    if inferred.is_empty() {
        return;
    }
    let mut accepted = std::collections::BTreeSet::new();
    let mut pending = vec![inferred];
    let mut round = 0usize;
    while let Some(chunk) = pending.pop() {
        round += 1;
        let enabled = accepted
            .iter()
            .copied()
            .chain(chunk.iter().copied())
            .collect();
        let Some(candidate_source) = rewrite_selected(&module.source, sites, chosen, &enabled)
        else {
            break;
        };
        let after = check_diagnostics(
            module,
            &candidate_source,
            module_graph,
            analysis,
            &config,
            round,
        );
        if !has_new_diagnostics(&baseline, &after) {
            accepted.extend(chunk);
        } else if chunk.len() > 1 {
            let middle = chunk.len() / 2;
            pending.push(chunk[middle..].to_vec());
            pending.push(chunk[..middle].to_vec());
        }
    }
    for (index, inference) in chosen.iter_mut().enumerate() {
        if inference.cause.is_inferred() && !accepted.contains(&index) {
            *inference = Inference::rejected();
        }
    }
}

/// Diagnostic counts keyed by code and message, so inserted bytes do not make
/// an existing diagnostic look new.
type DiagnosticIndex = BTreeMap<(Code, String), usize>;

fn check_diagnostics(
    module: &Module,
    source: &str,
    module_graph: &harn_modules::ModuleGraph,
    analysis: &mut AnalysisDatabase,
    config: &package::CheckConfig,
    round: usize,
) -> DiagnosticIndex {
    let id = SourceId::new(format!(
        "{}#annotate-{round}",
        module.path.to_string_lossy()
    ));
    analysis.set_source(id.clone(), source.to_string(), SourceVersion(1));
    let typecheck_config = commands::check::typecheck_config(&module.path, config, module_graph);
    let output = analysis.typecheck(&id, typecheck_config);
    analysis.remove_source(&id);
    let Ok(output) = output else {
        return DiagnosticIndex::new();
    };
    let mut index = DiagnosticIndex::new();
    for diagnostic in output.diagnostics {
        // Only errors can turn a working file into a broken one. A warning the
        // annotation newly reveals (an untyped dict read, say) is the checker
        // seeing more than it used to, which is the point of the migration.
        if diagnostic.code == Code::ImplicitAnyParameter
            || diagnostic.severity != harn_parser::DiagnosticSeverity::Error
        {
            continue;
        }
        *index
            .entry((diagnostic.code, diagnostic.message))
            .or_default() += 1;
    }
    index
}

fn has_new_diagnostics(baseline: &DiagnosticIndex, after: &DiagnosticIndex) -> bool {
    after
        .iter()
        .any(|(key, count)| *count > baseline.get(key).copied().unwrap_or_default())
}

/// Splice every annotation into `source`, using `unknown` outside `enabled`.
fn rewrite_selected(
    source: &str,
    sites: &[UnannotatedParam],
    chosen: &[Inference],
    enabled: &std::collections::BTreeSet<usize>,
) -> Option<String> {
    let mut insertions: Vec<(usize, String)> = Vec::with_capacity(sites.len());
    for (index, (site, inference)) in sites.iter().zip(chosen).enumerate() {
        let offset = param_annotations::annotation_insert_offset(source, site)?;
        let rendered = if enabled.contains(&index) {
            inference.rendered.as_str()
        } else {
            "unknown"
        };
        insertions.push((offset, format!(": {rendered}")));
    }
    insertions.sort_by_key(|(offset, _)| *offset);
    let mut out = String::with_capacity(source.len() + insertions.len() * 8);
    let mut cursor = 0usize;
    for (offset, text) in &insertions {
        out.push_str(source.get(cursor..*offset)?);
        out.push_str(text);
        cursor = *offset;
    }
    out.push_str(source.get(cursor..)?);
    Some(out)
}

fn annotation_edit(source: &str, site: &UnannotatedParam, annotation: &str) -> Option<FixEdit> {
    let offset = param_annotations::annotation_insert_offset(source, site)?;
    Some(FixEdit {
        span: Span::with_offsets(offset, offset, site.span.line, site.span.column),
        replacement: format!(": {annotation}"),
    })
}

fn candidate(
    file: &Path,
    site: &UnannotatedParam,
    annotation: &str,
    cause: Cause,
    edit: FixEdit,
) -> RepairCandidate {
    RepairCandidate {
        file: file.to_string_lossy().into_owned(),
        source: "typecheck",
        severity: "error",
        code: Code::ImplicitAnyParameter,
        message: format!(
            "{} `{}` parameter `{}` has no type annotation",
            site.kind.as_str(),
            site.owner,
            site.name
        ),
        unresolved_name: None,
        expected_type: None,
        span: Some(site.span),
        repair: Repair {
            id: RepairId::from_static("types/annotate-parameter"),
            summary: format!("Annotate `{}` as `{annotation}`", site.name),
            safety: RepairSafety::SurfaceChanging,
        },
        impact: RepairImpactWire {
            classification: "parameter-annotation".to_string(),
            strategy: Some("infer-from-body-and-call-sites".to_string()),
            signature_changes: Vec::new(),
            requires_cross_module_caller_updates: false,
            notes: vec![format!("inference: {}", cause.as_str())],
        },
        edits: vec![edit],
    }
}

impl From<&AnnotationResidue> for super::ParameterAnnotationsWire {
    fn from(residue: &AnnotationResidue) -> Self {
        super::ParameterAnnotationsWire {
            total: residue.total(),
            inferred: residue.inferred,
            unresolved: residue.unresolved,
            unresolved_share: (residue.unresolved_share() * 10_000.0).round() / 10_000.0,
            causes: residue
                .causes
                .iter()
                .map(|(cause, count)| ((*cause).to_string(), *count))
                .collect(),
        }
    }
}
