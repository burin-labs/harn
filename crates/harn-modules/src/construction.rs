//! Source retention and projections for the shared graph walk.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use harn_parser::SNode;

use super::{
    build_inner, normalize_path, ModuleGraph, ModuleGraphBuild, PackageContext,
    ParsedSourceRetention,
};

/// Build a module graph from a set of files.
///
/// Files referenced via `import` statements are loaded recursively so the
/// graph contains every module reachable from the seed set. Cycles and
/// already-loaded files are skipped via a visited set.
pub fn build(files: &[PathBuf]) -> ModuleGraph {
    build_inner(
        files,
        ParsedSourceRetention::None,
        None,
        PackageContext::Project,
        None,
    )
    .graph
}

/// Build a module graph while retaining parsed sources for the seed files.
///
/// Imported-only modules still participate in the graph, but their ASTs are
/// dropped after graph extraction so callers do not pay extra peak memory for
/// parsed sources they will not reuse.
pub fn build_with_parsed_sources(files: &[PathBuf]) -> ModuleGraphBuild {
    let parsed_source_targets = files.iter().map(|file| normalize_path(file)).collect();
    build_inner(
        files,
        ParsedSourceRetention::Seeds(&parsed_source_targets),
        None,
        PackageContext::Project,
        None,
    )
}

/// Build a closed module graph while retaining every reachable parsed source.
///
/// This is intentionally separate from editor-oriented graph construction:
/// package linking consumes every module AST, while ordinary diagnostics should
/// not retain an entire dependency graph after extracting its public surface.
pub fn build_closed_program(files: &[PathBuf]) -> ModuleGraphBuild {
    build_inner(
        files,
        ParsedSourceRetention::All,
        None,
        PackageContext::Project,
        None,
    )
}

/// Project each readable module before its source and syntax tree are dropped.
///
/// The callback runs once per canonical module, including imported modules,
/// and may run concurrently on parse workers. It borrows the same source and
/// program used to extract the graph. Retain only the facts the consumer needs
/// instead of keeping every reachable syntax tree alive until the walk ends.
/// `program` is `None` when lexing or parsing failed; the original source is
/// still available so consumers can preserve that failure's source identity.
pub fn build_with_module_projection<T: Send>(
    files: &[PathBuf],
    project: impl Fn(&Path, &str, Option<&[SNode]>) -> T + Sync,
) -> (ModuleGraph, HashMap<PathBuf, T>) {
    let projections = std::sync::Mutex::new(HashMap::new());
    let observer = |path: &Path, source: &str, program: Option<&[SNode]>| {
        let projection = project(path, source, program);
        projections
            .lock()
            .expect("module projection lock poisoned")
            .insert(path.to_path_buf(), projection);
    };
    let graph = build_inner(
        files,
        ParsedSourceRetention::None,
        None,
        PackageContext::Project,
        Some(&observer),
    )
    .graph;
    (
        graph,
        projections
            .into_inner()
            .expect("module projection lock poisoned"),
    )
}

/// Build a module graph that retains every reachable AST and honors unsaved
/// buffer overrides.
///
/// This is the construction find-references and `harn graph` share: one
/// resolution graph, one inverse index. `source_overrides` replace disk
/// contents for the named files so an editor buffer wins over the file it
/// just edited.
pub fn build_for_reference_index(
    files: &[PathBuf],
    source_overrides: Option<&HashMap<PathBuf, String>>,
) -> ModuleGraphBuild {
    build_inner(
        files,
        ParsedSourceRetention::All,
        source_overrides,
        PackageContext::Project,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_and_graph_share_the_parsed_source_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let paths = (0..12)
            .map(|index| {
                let path = dir.path().join(format!("module{index}.harn"));
                std::fs::write(&path, "pub fn original() { 1 }\n").unwrap();
                path
            })
            .collect::<Vec<_>>();
        let observed = std::sync::atomic::AtomicUsize::new(0);
        let (graph, projections) = build_with_module_projection(&paths, |path, source, program| {
            observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            assert_eq!(program.unwrap().len(), 1);
            // A change after this parse cannot change either projection.
            std::fs::write(path, "pub fn replacement() { 2 }\n").unwrap();
            source.to_owned()
        });
        assert_eq!(
            observed.load(std::sync::atomic::Ordering::Relaxed),
            paths.len()
        );
        assert_eq!(graph.module_paths().len(), paths.len());
        assert_eq!(projections.len(), paths.len());
        for path in &paths {
            assert_eq!(
                projections[&normalize_path(path)],
                "pub fn original() { 1 }\n"
            );
            assert_eq!(
                graph.exported_kind(path, "original"),
                Some(super::super::DefKind::Function)
            );
            assert_eq!(graph.exported_kind(path, "replacement"), None);
        }
        let refreshed = build(&paths);
        for path in &paths {
            assert_eq!(refreshed.exported_kind(path, "original"), None);
            assert_eq!(
                refreshed.exported_kind(path, "replacement"),
                Some(super::super::DefKind::Function)
            );
        }
    }
}
