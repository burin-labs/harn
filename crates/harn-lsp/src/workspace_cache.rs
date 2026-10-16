//! Analysis inputs shared by every open document: each root's module graph,
//! the project lint configuration, and the editor's unsaved buffers.
//!
//! A document's module graph depends on its own import declarations and on
//! the modules it reaches. Keying the cached graph by the root's import
//! declarations means an edit that leaves the imports alone reuses the graph
//! without reading a single imported module from disk. A change to any module
//! the graph reached drops the entry, and [`WorkspaceCache::invalidate_module`]
//! returns the roots it dropped so the server can re-check those importers.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use harn_modules::project_config::LintConfig;
use harn_modules::ModuleGraph;
use harn_parser::{Node, SNode};

struct CachedGraph {
    imports_key: u64,
    graph: Arc<ModuleGraph>,
    valid: bool,
}

#[derive(Default)]
pub(crate) struct WorkspaceCache {
    graphs: Mutex<HashMap<PathBuf, CachedGraph>>,
    lint_configs: Mutex<HashMap<PathBuf, Arc<LintConfig>>>,
    buffers: Mutex<HashMap<PathBuf, String>>,
    generation: AtomicU64,
    #[cfg(test)]
    graph_probe: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl WorkspaceCache {
    /// The module graph for `path`, built against the editor's unsaved
    /// buffers and reused while the root's import declarations are unchanged.
    pub(crate) fn module_graph(
        &self,
        path: &Path,
        source: &str,
        program: &[SNode],
    ) -> Arc<ModuleGraph> {
        let imports_key = imports_key(source, program);
        loop {
            let (generation, mut sources) = {
                let graphs = lock(&self.graphs);
                if let Some(cached) = graphs.get(path) {
                    if cached.valid && cached.imports_key == imports_key {
                        return Arc::clone(&cached.graph);
                    }
                }
                (self.generation(), lock(&self.buffers).clone())
            };
            sources.insert(path.to_path_buf(), source.to_string());
            let graph = Arc::new(harn_modules::build_with_sources(path, &sources));
            #[cfg(test)]
            if let Some(probe) = lock(&self.graph_probe).clone() {
                probe();
            }
            let mut graphs = lock(&self.graphs);
            if generation != self.generation() {
                continue;
            }
            graphs.insert(
                path.to_path_buf(),
                CachedGraph {
                    imports_key,
                    graph: Arc::clone(&graph),
                    valid: true,
                },
            );
            return graph;
        }
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// The `[lint]` table of the project that owns `path`, read once per
    /// directory until [`Self::invalidate_all`].
    pub(crate) fn lint_config(&self, path: &Path) -> Arc<LintConfig> {
        let dir = path.parent().unwrap_or(path).to_path_buf();
        loop {
            let generation = self.generation();
            if let Some(config) = lock(&self.lint_configs).get(&dir) {
                return Arc::clone(config);
            }
            let config = Arc::new(
                harn_modules::project_config::load_for_path(path)
                    .map(|config| config.lint)
                    .unwrap_or_default(),
            );
            let mut configs = lock(&self.lint_configs);
            if generation != self.generation() {
                continue;
            }
            configs.insert(dir, Arc::clone(&config));
            return config;
        }
    }

    /// Record an open document's text, which every graph build uses in place
    /// of the file on disk.
    pub(crate) fn set_buffer(&self, path: PathBuf, source: String) {
        let mut graphs = lock(&self.graphs);
        lock(&self.buffers).insert(path.clone(), source);
        self.generation.fetch_add(1, Ordering::Release);
        invalidate_graphs(&mut graphs, &path);
    }

    pub(crate) fn remove_buffer(&self, path: &Path) {
        let mut graphs = lock(&self.graphs);
        lock(&self.buffers).remove(path);
        graphs.remove(path);
        self.generation.fetch_add(1, Ordering::Release);
        invalidate_graphs(&mut graphs, path);
    }

    /// Invalidate graphs that reached `changed`, preserving their dependency
    /// paths until they rebuild so concurrent notifications still find them.
    pub(crate) fn invalidate_module(&self, changed: &Path) -> Vec<PathBuf> {
        let mut graphs = lock(&self.graphs);
        self.generation.fetch_add(1, Ordering::Release);
        invalidate_graphs(&mut graphs, changed)
    }

    /// Forget every graph and project configuration, after an external file
    /// change or a configuration change the cache cannot attribute.
    pub(crate) fn invalidate_all(&self) {
        let mut graphs = lock(&self.graphs);
        graphs.clear();
        lock(&self.lint_configs).clear();
        self.generation.fetch_add(1, Ordering::Release);
    }
}

fn invalidate_graphs(graphs: &mut HashMap<PathBuf, CachedGraph>, changed: &Path) -> Vec<PathBuf> {
    graphs
        .iter()
        .filter(|(root, cached)| root.as_path() != changed && cached.graph.contains_module(changed))
        .map(|(root, _)| root.clone())
        .collect::<Vec<_>>()
        .into_iter()
        .inspect(|root| graphs.get_mut(root).unwrap().valid = false)
        .collect()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A digest of the root's import declarations and where they sit, the only
/// part of the root the graph-derived facts the editor reads depend on:
/// imported declarations, and selective-import issues anchored at those spans.
fn imports_key(source: &str, program: &[SNode]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for node in program {
        if matches!(
            node.node,
            Node::ImportDecl { .. } | Node::SelectiveImport { .. } | Node::NamespaceImport { .. }
        ) {
            (node.span.start, source.get(node.span.start..node.span.end)).hash(&mut hasher);
        }
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use tower_lsp::lsp_types::Url;

    use super::WorkspaceCache;
    use crate::document::DocumentState;

    fn analyze(state: &mut DocumentState, uri: &Url, workspace: Option<&WorkspaceCache>) {
        state.dirty = true;
        state.reparse_if_dirty_with_rules(Some(uri), None, workspace);
    }

    /// An edit that leaves the imports alone reuses the module graph, so the
    /// imported module is not read from disk again; a change to that module
    /// drops the graph and the next analysis reads it once.
    #[test]
    fn edits_reuse_the_module_graph_until_an_imported_module_changes() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib.harn");
        let app = dir.path().join("app.harn");
        std::fs::write(&lib, "pub fn helper() -> int {\n  return 1\n}\n").unwrap();
        let source = "import { helper } from \"./lib\"\n\npipeline main() {\n  log(helper())\n}\n";
        std::fs::write(&app, source).unwrap();
        let uri = Url::from_file_path(&app).unwrap();
        let workspace = WorkspaceCache::default();
        let mut state = DocumentState::new_for_language(source.to_string(), "harn", &uri);
        let reads = harn_modules::module_source_disk_reads;

        let start = reads();
        analyze(&mut state, &uri, Some(&workspace));
        assert_eq!(
            reads() - start,
            1,
            "the first analysis reads the imported module"
        );

        let edited = source.replace("log(helper())", "log(helper() + 1)");
        state.update_source(edited);
        let start = reads();
        analyze(&mut state, &uri, Some(&workspace));
        assert_eq!(reads() - start, 0, "a body edit reuses the cached graph");

        // Without the cache every analysis rebuilds the graph from disk.
        let start = reads();
        analyze(&mut state, &uri, None);
        assert_eq!(reads() - start, 1);

        std::fs::write(&lib, "pub fn helper() -> string {\n  return \"one\"\n}\n").unwrap();
        let importers = workspace.invalidate_module(&lib);
        assert_eq!(importers, vec![app]);
        let start = reads();
        analyze(&mut state, &uri, Some(&workspace));
        assert_eq!(
            reads() - start,
            1,
            "the importer re-reads the changed module"
        );
    }

    #[test]
    fn a_graph_built_during_a_dependency_edit_is_rebuilt_before_publication() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib.harn");
        let app = dir.path().join("app.harn");
        std::fs::write(&lib, "pub fn helper() -> int { return 1 }\n").unwrap();
        let source =
            "import { helper } from \"./lib\"\npub fn total() -> int { return helper() }\n";
        std::fs::write(&app, source).unwrap();
        let uri = Url::from_file_path(&app).unwrap();
        let workspace = Arc::new(WorkspaceCache::default());
        let reached = Arc::new(AtomicBool::new(false));
        let weak = Arc::downgrade(&workspace);
        let observed = Arc::clone(&reached);
        *super::lock(&workspace.graph_probe) = Some(Arc::new(move || {
            if !observed.swap(true, Ordering::SeqCst) {
                weak.upgrade().unwrap().set_buffer(
                    lib.clone(),
                    "pub fn helper() -> string { return \"changed\" }\n".into(),
                );
            }
        }));
        let mut state = DocumentState::new_for_language(source.into(), "harn", &uri);
        analyze(&mut state, &uri, Some(&workspace));
        assert!(
            reached.load(Ordering::SeqCst),
            "the edit occurred after a real graph build"
        );
        assert!(state
            .type_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == harn_parser::DiagnosticSeverity::Error));
        assert!(workspace
            .invalidate_module(&dir.path().join("lib.harn"))
            .contains(&app));
    }
}
