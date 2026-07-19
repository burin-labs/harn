use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::package_imports::{acquire_package_snapshots, resolve_import_path_with_snapshots};
use crate::package_snapshot::PackageSnapshot;
use harn_lexer::Span;
use harn_parser::{BindingPattern, Node, Parser, SNode};

pub mod asset_paths;
mod declarations;
pub mod fingerprint;
pub mod package_execution;
mod package_imports;
pub mod package_snapshot;
pub mod personas;
pub mod project_config;
mod stdlib;

pub use declarations::{public_declarations, DefKind, PublicDeclaration};
pub use package_imports::{
    resolve_import_path, resolve_import_path_with_guard, resolve_import_path_with_snapshot,
};

/// A resolved definition site within a module.
#[derive(Debug, Clone)]
pub struct DefSite {
    pub name: String,
    pub file: PathBuf,
    pub kind: DefKind,
    pub span: Span,
}

/// Wildcard import resolution status for a single importing module.
#[derive(Debug, Clone)]
pub enum WildcardResolution {
    /// Resolved all wildcard imports and can expose wildcard exports.
    Resolved(HashSet<String>),
    /// At least one wildcard import could not be resolved.
    Unknown,
}

/// Parsed information for a set of module files.
#[derive(Debug, Default)]
pub struct ModuleGraph {
    modules: HashMap<PathBuf, ModuleInfo>,
    // Resolved definition/import paths remain valid for the graph lifetime.
    _package_snapshots: Vec<PackageSnapshot>,
}

#[derive(Debug, Clone)]
pub struct ParsedModuleSource {
    pub source: String,
    pub program: Vec<SNode>,
}

#[derive(Debug, Default)]
pub struct ModuleGraphBuild {
    pub graph: ModuleGraph,
    pub parsed_sources: HashMap<PathBuf, ParsedModuleSource>,
}

#[derive(Debug, Default)]
struct ModuleInfo {
    /// All declarations visible in this module (for local symbol lookup and
    /// go-to-definition resolution).
    declarations: HashMap<String, DefSite>,
    /// Names exported by this module after re-export resolution. Equal to
    /// [`own_exports`] union the keys of [`selective_re_exports`] union the
    /// transitive exports of [`wildcard_re_export_paths`]. Populated in
    /// `build()` after all modules are loaded.
    exports: HashSet<String>,
    /// Names declared locally and exported by this module — i.e. `pub fn`,
    /// `pub struct`, etc.
    own_exports: HashSet<String>,
    /// Selective re-exports introduced by `pub import { name } from "..."`.
    /// Maps the re-exported name to every canonical source module path it
    /// could originate from. Multiple entries per name indicate a conflict
    /// (`pub import { foo } from "a"` and `pub import { foo } from "b"`)
    /// and are surfaced by [`ModuleGraph::re_export_conflicts`]. Lookup
    /// callers (e.g. go-to-definition) follow the first recorded source.
    selective_re_exports: HashMap<String, Vec<PathBuf>>,
    /// Wildcard re-exports introduced by `pub import "..."`. Each entry is
    /// the canonical path of a module whose entire public export surface
    /// this module re-exports.
    wildcard_re_export_paths: Vec<PathBuf>,
    /// Names introduced by selective imports across this module.
    selective_import_names: HashSet<String>,
    /// Import references encountered in this file.
    imports: Vec<ImportRef>,
    /// True when at least one wildcard import could not be resolved.
    has_unresolved_wildcard_import: bool,
    /// True when at least one selective import could not be resolved
    /// (importing file path missing). Prevents `imported_names_for_file`
    /// from returning a partial answer when any import is broken.
    has_unresolved_selective_import: bool,
    /// Top-level type-like declarations that can be imported into a caller's
    /// static type environment.
    type_declarations: Vec<SNode>,
    /// Top-level callable declarations whose signatures can be imported into
    /// a caller's static type environment.
    callable_declarations: Vec<SNode>,
    /// Set when this module's own source failed to lex or parse. The module is
    /// still recorded in the graph (with an otherwise-empty surface) so that
    /// importers can be told their target is broken — instead of silently
    /// seeing zero exports and mislabeling the imported symbol as "undefined"
    /// at the call site.
    load_error: Option<ModuleLoadError>,
}

/// A lex/parse failure captured while loading a module into the graph.
///
/// Retained so that `harn check <consumer>` can surface the real error in an
/// imported file rather than downgrading its exports to "undefined" at the
/// consumer's call site.
#[derive(Debug, Clone)]
pub struct ModuleLoadError {
    /// Rendered lex/parse error message (includes the failing line:column).
    pub message: String,
    /// Span of the failure within the imported module's own source.
    pub span: Span,
}

/// A consumer import whose resolved target module failed to compile. Reported
/// by [`ModuleGraph::import_compile_failures`].
#[derive(Debug, Clone)]
pub struct ImportCompileFailure {
    /// The import path exactly as written in the consumer.
    pub import_raw_path: String,
    /// Span of the consumer's `import` statement.
    pub import_span: Span,
    /// Canonical path of the broken imported module.
    pub module_path: PathBuf,
    /// The imported module's real lex/parse error.
    pub error: ModuleLoadError,
}

#[derive(Debug, Clone)]
struct ImportRef {
    raw_path: String,
    path: Option<PathBuf>,
    selective_names: Option<HashSet<String>>,
    import_span: Span,
}

/// Public import edge summary for static module graph consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleImport {
    /// The import string as written in source.
    pub raw_path: String,
    /// Resolved module path when the import could be resolved.
    pub resolved_path: Option<PathBuf>,
    /// `None` for wildcard imports; otherwise the selected names.
    pub selective_names: Option<Vec<String>>,
}

/// Return the source for a resolved module path.
///
/// Real paths are read from disk. `<std>/<module>` virtual paths are backed by
/// the embedded stdlib source table, so callers can parse resolved stdlib
/// modules without knowing about the stdlib mirror layout.
pub fn read_module_source(path: &Path) -> Option<String> {
    if let Some(stdlib_module) = stdlib_module_from_path(path) {
        return stdlib::get_stdlib_source(stdlib_module).map(ToString::to_string);
    }
    std::fs::read_to_string(path).ok()
}

/// Build a module graph from a set of files.
///
/// Files referenced via `import` statements are loaded recursively so the
/// graph contains every module reachable from the seed set. Cycles and
/// already-loaded files are skipped via a visited set.
pub fn build(files: &[PathBuf]) -> ModuleGraph {
    build_inner(files, None, None).graph
}

/// Build a module graph using caller-owned source for one root file.
///
/// Imported modules still resolve from their normal filesystem or embedded
/// stdlib locations. This keeps editor diagnostics aligned with unsaved root
/// buffers without creating a second module resolver.
pub fn build_with_source(file: &Path, source: &str) -> ModuleGraph {
    let file = normalize_path(file);
    let source_overrides = HashMap::from([(file.clone(), source.to_string())]);
    build_inner(&[file], None, Some(&source_overrides)).graph
}

/// Build a module graph while retaining parsed sources for the seed files.
///
/// Imported-only modules still participate in the graph, but their ASTs are
/// dropped after graph extraction so callers do not pay extra peak memory for
/// parsed sources they will not reuse.
pub fn build_with_parsed_sources(files: &[PathBuf]) -> ModuleGraphBuild {
    let parsed_source_targets = files.iter().map(|file| normalize_path(file)).collect();
    build_inner(files, Some(&parsed_source_targets), None)
}

fn build_inner(
    files: &[PathBuf],
    parsed_source_targets: Option<&HashSet<PathBuf>>,
    source_overrides: Option<&HashMap<PathBuf, String>>,
) -> ModuleGraphBuild {
    let package_snapshots = acquire_package_snapshots(files);
    let mut modules: HashMap<PathBuf, ModuleInfo> = HashMap::new();
    let mut parsed_sources: HashMap<PathBuf, ParsedModuleSource> = HashMap::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut wave: Vec<PathBuf> = Vec::new();
    for file in files {
        let canonical = normalize_path(file);
        if seen.insert(canonical.clone()) {
            wave.push(canonical);
        }
    }
    // Breadth-first over import waves. Every path in a wave is new (the
    // `seen` set dedupes before enqueue), and `load_module` is a pure
    // read+lex+parse+extract, so each wave loads in parallel; discovery of
    // the next wave stays sequential to keep the dedup deterministic. A
    // whole-tree seed set arrives as one large first wave, which is where
    // nearly all the parse work is, so the serial-BFS tail on deep import
    // chains does not matter in practice.
    while !wave.is_empty() {
        let loaded = load_wave(&wave, &package_snapshots, source_overrides);
        let mut next_wave: Vec<PathBuf> = Vec::new();
        for (path, (module, parsed)) in wave.drain(..).zip(loaded) {
            let retain_parsed_source =
                parsed_source_targets.is_some_and(|targets| targets.contains(&path));
            if retain_parsed_source {
                if let Some(parsed) = parsed {
                    parsed_sources.insert(path.clone(), parsed);
                }
            }
            // Enqueue resolved import targets so the whole reachable graph is
            // discovered without the caller having to pre-walk imports.
            //
            // `resolve_import_path` returns paths as `base.join(import)` —
            // i.e. with `..` segments preserved rather than collapsed. If we
            // dedupe on those raw forms, two files that import each other
            // across sibling dirs (`lib/context/` ↔ `lib/runtime/`) produce a
            // different path spelling on every cycle — `.../context/../runtime/`,
            // then `.../context/../runtime/../context/`, and so on — each of
            // which is treated as a new file. The walk only terminates when
            // `path.exists()` starts failing at the filesystem's `PATH_MAX`,
            // which is 1024 on macOS but 4096 on Linux. Linux therefore
            // re-parses the same handful of files thousands of times, balloons
            // RSS into the multi-GB range, and gets SIGKILL'd by CI runners.
            // Canonicalize once here so `seen` dedupes by the underlying file,
            // not by its path spelling.
            for import in &module.imports {
                if let Some(import_path) = &import.path {
                    let canonical = normalize_path(import_path);
                    if seen.insert(canonical.clone()) {
                        next_wave.push(canonical);
                    }
                }
            }
            modules.insert(path, module);
        }
        wave = next_wave;
    }
    resolve_re_exports(&mut modules);
    ModuleGraphBuild {
        graph: ModuleGraph {
            modules,
            _package_snapshots: package_snapshots,
        },
        parsed_sources,
    }
}

/// Environment override for the graph-build worker-pool size. `1` forces the
/// serial walk; unset defaults to the machine's available parallelism.
pub const MODULE_GRAPH_JOBS_ENV: &str = "HARN_MODULE_GRAPH_JOBS";

/// Load one BFS wave of module paths, in parallel when the wave is large
/// enough to pay for the threads. Results are index-aligned with `paths`.
fn load_wave(
    paths: &[PathBuf],
    package_snapshots: &[PackageSnapshot],
    source_overrides: Option<&HashMap<PathBuf, String>>,
) -> Vec<(ModuleInfo, Option<ParsedModuleSource>)> {
    const MIN_PARALLEL_WAVE: usize = 8;
    let configured = std::env::var(MODULE_GRAPH_JOBS_ENV)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|&jobs| jobs > 0);
    let workers = configured
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(std::num::NonZeroUsize::get)
                .unwrap_or(1)
        })
        .min(paths.len());
    if workers <= 1 || paths.len() < MIN_PARALLEL_WAVE {
        return paths
            .iter()
            .map(|path| load_module(path, package_snapshots, source_overrides))
            .collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut produced: Vec<(usize, (ModuleInfo, Option<ParsedModuleSource>))> =
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..workers)
                .map(|_| {
                    scope.spawn(|| {
                        let mut local = Vec::new();
                        loop {
                            let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let Some(path) = paths.get(index) else {
                                break;
                            };
                            local.push((
                                index,
                                load_module(path, package_snapshots, source_overrides),
                            ));
                        }
                        local
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|handle| match handle.join() {
                    Ok(local) => local,
                    Err(panic) => std::panic::resume_unwind(panic),
                })
                .collect()
        });
    produced.sort_unstable_by_key(|(index, _)| *index);
    produced.into_iter().map(|(_, loaded)| loaded).collect()
}

/// Iteratively expand each module's `exports` set to include the transitive
/// public surface of its `pub import "..."` re-export targets. Cycles are
/// safe because the loop only adds names — once no module's set grows in a
/// pass, the fixpoint is reached.
fn resolve_re_exports(modules: &mut HashMap<PathBuf, ModuleInfo>) {
    let keys: Vec<PathBuf> = modules.keys().cloned().collect();
    loop {
        let mut changed = false;
        for path in &keys {
            // Snapshot the wildcard target list and gather the union of
            // their current exports without holding a mutable borrow.
            let wildcard_paths = modules
                .get(path)
                .map(|m| m.wildcard_re_export_paths.clone())
                .unwrap_or_default();
            if wildcard_paths.is_empty() {
                continue;
            }
            let mut additions: Vec<String> = Vec::new();
            for src in &wildcard_paths {
                let src_canonical = normalize_path(src);
                if let Some(src_module) = modules.get(src).or_else(|| modules.get(&src_canonical)) {
                    additions.extend(src_module.exports.iter().cloned());
                }
            }
            if let Some(module) = modules.get_mut(path) {
                for name in additions {
                    if module.exports.insert(name) {
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
}

impl ModuleGraph {
    /// Sorted list of every module path discovered by [`build`]. Includes
    /// `<std>/<name>` virtual paths for stdlib modules reached transitively.
    /// Callers that want only real-disk modules can filter for paths whose
    /// string form does not start with `<std>/`.
    pub fn module_paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = self.modules.keys().cloned().collect();
        paths.sort();
        paths
    }

    /// True when `path` (or its canonical form) was discovered during the
    /// module-graph walk.
    pub fn contains_module(&self, path: &Path) -> bool {
        self.modules.contains_key(path) || self.modules.contains_key(&normalize_path(path))
    }

    /// Collect every name used in selective imports from all files.
    pub fn all_selective_import_names(&self) -> HashSet<&str> {
        let mut names = HashSet::new();
        for module in self.modules.values() {
            for name in &module.selective_import_names {
                names.insert(name.as_str());
            }
        }
        names
    }

    /// Files that directly import `target`. Resolves `target` to a
    /// canonical path before lookup so callers can pass either spelling.
    pub fn importers_of(&self, target: &Path) -> Vec<PathBuf> {
        let target = normalize_path(target);
        let mut out: Vec<PathBuf> = self
            .modules
            .iter()
            .filter(|(_, info)| {
                info.imports.iter().any(|import| {
                    import
                        .path
                        .as_ref()
                        .is_some_and(|p| normalize_path(p) == target)
                })
            })
            .map(|(path, _)| path.clone())
            .collect();
        out.sort();
        out
    }

    /// Import edges declared by `file`, sorted by raw path and selected names.
    pub fn imports_for_module(&self, file: &Path) -> Vec<ModuleImport> {
        let file = normalize_path(file);
        let Some(module) = self.modules.get(&file) else {
            return Vec::new();
        };
        let mut imports: Vec<ModuleImport> = module
            .imports
            .iter()
            .map(|import| {
                let mut selective_names = import
                    .selective_names
                    .as_ref()
                    .map(|names| names.iter().cloned().collect::<Vec<_>>());
                if let Some(names) = selective_names.as_mut() {
                    names.sort();
                }
                ModuleImport {
                    raw_path: import.raw_path.clone(),
                    resolved_path: import.path.as_ref().map(|path| normalize_path(path)),
                    selective_names,
                }
            })
            .collect();
        imports.sort_by(|left, right| {
            left.raw_path
                .cmp(&right.raw_path)
                .then_with(|| left.selective_names.cmp(&right.selective_names))
                .then_with(|| left.resolved_path.cmp(&right.resolved_path))
        });
        imports
    }

    /// Exported symbol names for `file`, sorted alphabetically.
    pub fn exports_for_module(&self, file: &Path) -> Vec<String> {
        let file = normalize_path(file);
        let Some(module) = self.modules.get(&file) else {
            return Vec::new();
        };
        let mut exports: Vec<String> = module.exports.iter().cloned().collect();
        exports.sort();
        exports
    }

    /// Resolve wildcard imports for `file`.
    ///
    /// Returns `Unknown` when any wildcard import cannot be resolved, because
    /// callers should conservatively disable wildcard-import-sensitive checks.
    pub fn wildcard_exports_for(&self, file: &Path) -> WildcardResolution {
        let file = normalize_path(file);
        let Some(module) = self.modules.get(&file) else {
            return WildcardResolution::Unknown;
        };
        if module.has_unresolved_wildcard_import {
            return WildcardResolution::Unknown;
        }

        let mut names = HashSet::new();
        for import in module
            .imports
            .iter()
            .filter(|import| import.selective_names.is_none())
        {
            let Some(import_path) = &import.path else {
                return WildcardResolution::Unknown;
            };
            let imported = self.modules.get(import_path).or_else(|| {
                let normalized = normalize_path(import_path);
                self.modules.get(&normalized)
            });
            let Some(imported) = imported else {
                return WildcardResolution::Unknown;
            };
            names.extend(imported.exports.iter().cloned());
        }
        WildcardResolution::Resolved(names)
    }

    /// Collect every statically callable/referenceable name introduced into
    /// `file` by its imports.
    ///
    /// Returns `Some` only when **every** import (wildcard or selective) in
    /// `file` is fully resolvable via the graph. Returns `None` when any
    /// import is unresolved, so callers can fall back to conservative
    /// behavior instead of emitting spurious "undefined name" errors.
    ///
    /// The returned set contains:
    /// - all public exports from wildcard-imported modules (transitively
    ///   following `pub import` re-export chains), and
    /// - selectively imported names that the target module actually exports
    ///   (its `pub` surface or re-exports) — matching what the VM accepts at
    ///   runtime. A name that exists only privately in the target is NOT
    ///   importable.
    ///
    /// Every import in `file` whose resolved target module failed to lex or
    /// parse. Lets `harn check <file>` surface the real error inside the
    /// imported module (anchored at the consumer's `import` statement) instead
    /// of downgrading the imported symbols to "undefined" at their call sites.
    #[must_use]
    pub fn import_compile_failures(&self, file: &Path) -> Vec<ImportCompileFailure> {
        let file = normalize_path(file);
        let Some(module) = self.modules.get(&file) else {
            return Vec::new();
        };
        let mut failures = Vec::new();
        for import in &module.imports {
            let Some(import_path) = &import.path else {
                continue;
            };
            let Some(target) = self
                .modules
                .get(import_path)
                .or_else(|| self.modules.get(&normalize_path(import_path)))
            else {
                continue;
            };
            if let Some(error) = &target.load_error {
                failures.push(ImportCompileFailure {
                    import_raw_path: import.raw_path.clone(),
                    import_span: import.import_span,
                    module_path: normalize_path(import_path),
                    error: error.clone(),
                });
            }
        }
        failures
    }

    pub fn imported_names_for_file(&self, file: &Path) -> Option<HashSet<String>> {
        let file = normalize_path(file);
        let module = self.modules.get(&file)?;
        if module.has_unresolved_wildcard_import || module.has_unresolved_selective_import {
            return None;
        }

        let mut names = HashSet::new();
        for import in &module.imports {
            let import_path = import.path.as_ref()?;
            let imported = self
                .modules
                .get(import_path)
                .or_else(|| self.modules.get(&normalize_path(import_path)))?;
            // The target parsed nothing (lex/parse failure). Fall back to the
            // conservative `None` answer so the cross-module undefined-name
            // check stays silent — the real error is surfaced separately by
            // `import_compile_failures`, not mislabeled as an undefined symbol
            // at this consumer's call site.
            if imported.load_error.is_some() {
                return None;
            }
            match &import.selective_names {
                None => {
                    names.extend(imported.exports.iter().cloned());
                }
                Some(selective) => {
                    // A selectively imported name is in scope when it exists in
                    // the target module (as a declaration or a re-export). The
                    // stricter "must be `pub`" check is reported precisely at
                    // the import site by the `HARN-IMP-002` preflight scan
                    // (`scan_selective_import_visibility`) and enforced at load
                    // time, so it is intentionally *not* duplicated here —
                    // otherwise a private import would surface both an
                    // import-site and a redundant call-site error.
                    for name in selective {
                        if imported.declarations.contains_key(name)
                            || imported.exports.contains(name)
                        {
                            names.insert(name.clone());
                        }
                    }
                }
            }
        }
        Some(names)
    }

    /// Collect type / struct / enum / interface declarations made visible to
    /// `file` by its imports. Returns `None` when any import is unresolved so
    /// callers can fall back to conservative behavior.
    pub fn imported_type_declarations_for_file(&self, file: &Path) -> Option<Vec<SNode>> {
        let file = normalize_path(file);
        let module = self.modules.get(&file)?;
        if module.has_unresolved_wildcard_import || module.has_unresolved_selective_import {
            return None;
        }

        let mut decls = Vec::new();
        for import in &module.imports {
            let import_path = import.path.as_ref()?;
            let imported = self
                .modules
                .get(import_path)
                .or_else(|| self.modules.get(&normalize_path(import_path)))?;
            // The target parsed nothing (lex/parse failure). Fall back to the
            // conservative `None` answer so the cross-module undefined-name
            // check stays silent — the real error is surfaced separately by
            // `import_compile_failures`, not mislabeled as an undefined symbol
            // at this consumer's call site.
            if imported.load_error.is_some() {
                return None;
            }
            let names_to_collect: Vec<String> = match &import.selective_names {
                None => imported.exports.iter().cloned().collect(),
                Some(selective) => selective.iter().cloned().collect(),
            };
            for name in &names_to_collect {
                let mut visited = HashSet::new();
                if let Some(decl) = self.find_exported_type_decl(import_path, name, &mut visited) {
                    decls.push(decl);
                }
            }
            // Every type alias / struct / enum / interface declared in the
            // imported module is visible to the *typechecker*, `pub` or not:
            // an imported fn's signature may reference a module-private alias
            // ("options: PickKeysOptions"), and without its definition the
            // caller sees only a phantom `Named(...)` and skips contract
            // checks. This visibility is typing-only — name-level import
            // privacy is still enforced by `selective_import_issues`
            // and the runtime loader, which reject importing a non-`pub`
            // type by name.
            for ty_decl in &imported.type_declarations {
                if type_decl_name(ty_decl).is_some() {
                    decls.push(ty_decl.clone());
                }
            }
        }
        Some(decls)
    }

    /// Collect callable declarations made visible to `file` by its imports.
    /// Only signatures are consumed by the type checker; imported bodies
    /// remain owned by their defining modules.
    pub fn imported_callable_declarations_for_file(&self, file: &Path) -> Option<Vec<SNode>> {
        let file = normalize_path(file);
        let module = self.modules.get(&file)?;
        if module.has_unresolved_wildcard_import || module.has_unresolved_selective_import {
            return None;
        }

        let mut decls = Vec::new();
        for import in &module.imports {
            let import_path = import.path.as_ref()?;
            let imported = self
                .modules
                .get(import_path)
                .or_else(|| self.modules.get(&normalize_path(import_path)))?;
            // The target parsed nothing (lex/parse failure). Fall back to the
            // conservative `None` answer so the cross-module undefined-name
            // check stays silent — the real error is surfaced separately by
            // `import_compile_failures`, not mislabeled as an undefined symbol
            // at this consumer's call site.
            if imported.load_error.is_some() {
                return None;
            }
            let selective_import = import.selective_names.is_some();
            let names_to_collect: Vec<String> = match &import.selective_names {
                None => imported.exports.iter().cloned().collect(),
                Some(selective) => selective.iter().cloned().collect(),
            };
            for name in &names_to_collect {
                if selective_import || imported.own_exports.contains(name) {
                    if let Some(decl) = imported
                        .callable_declarations
                        .iter()
                        .find(|decl| callable_decl_name(decl) == Some(name.as_str()))
                    {
                        decls.push(decl.clone());
                        continue;
                    }
                }
                let mut visited = HashSet::new();
                if let Some(decl) =
                    self.find_exported_callable_decl(import_path, name, &mut visited)
                {
                    decls.push(decl);
                }
            }
        }
        Some(decls)
    }

    /// Walk a module's local type declarations and re-export chains to find
    /// the SNode for an exported type/struct/enum/interface named `name`.
    fn find_exported_type_decl(
        &self,
        path: &Path,
        name: &str,
        visited: &mut HashSet<PathBuf>,
    ) -> Option<SNode> {
        let canonical = normalize_path(path);
        if !visited.insert(canonical.clone()) {
            return None;
        }
        let module = self
            .modules
            .get(&canonical)
            .or_else(|| self.modules.get(path))?;
        for decl in &module.type_declarations {
            if type_decl_name(decl) == Some(name) && module.own_exports.contains(name) {
                return Some(decl.clone());
            }
        }
        if let Some(sources) = module.selective_re_exports.get(name) {
            for source in sources {
                if let Some(decl) = self.find_exported_type_decl(source, name, visited) {
                    return Some(decl);
                }
            }
        }
        for source in &module.wildcard_re_export_paths {
            if let Some(decl) = self.find_exported_type_decl(source, name, visited) {
                return Some(decl);
            }
        }
        None
    }

    fn find_exported_callable_decl(
        &self,
        path: &Path,
        name: &str,
        visited: &mut HashSet<PathBuf>,
    ) -> Option<SNode> {
        let canonical = normalize_path(path);
        if !visited.insert(canonical.clone()) {
            return None;
        }
        let module = self
            .modules
            .get(&canonical)
            .or_else(|| self.modules.get(path))?;
        for decl in &module.callable_declarations {
            if callable_decl_name(decl) == Some(name) && module.own_exports.contains(name) {
                return Some(decl.clone());
            }
        }
        if let Some(sources) = module.selective_re_exports.get(name) {
            for source in sources {
                if let Some(decl) = self.find_exported_callable_decl(source, name, visited) {
                    return Some(decl);
                }
            }
        }
        for source in &module.wildcard_re_export_paths {
            if let Some(decl) = self.find_exported_callable_decl(source, name, visited) {
                return Some(decl);
            }
        }
        None
    }

    /// Find the definition of `name` visible from `file`.
    ///
    /// Recurses through `pub import` re-export chains so go-to-definition
    /// lands on the symbol's actual declaration site instead of the facade
    /// module that forwarded it.
    pub fn definition_of(&self, file: &Path, name: &str) -> Option<DefSite> {
        let mut visited = HashSet::new();
        self.definition_of_inner(file, name, &mut visited)
    }

    /// Find the declaration that contributes exported `name` to `file`.
    ///
    /// Unlike [`Self::definition_of`], this ignores private local declarations
    /// and private imports. It follows only the module's public declaration and
    /// `pub import` graph, making it suitable for package manifests, API docs,
    /// and other consumers of a module's externally visible surface.
    pub fn export_definition_of(&self, file: &Path, name: &str) -> Option<DefSite> {
        let mut visited = HashSet::new();
        self.export_definition_of_inner(file, name, &mut visited)
    }

    /// Sorted names of every declaration recorded for `file` (functions,
    /// pipelines, tools, structs, ...). Used by the check-result cache to
    /// key the cross-file lint-exemption subset that applies to this file.
    pub fn declared_names_for_file(&self, file: &Path) -> Option<Vec<&str>> {
        let module = self.modules.get(&normalize_path(file))?;
        let mut names: Vec<&str> = module.declarations.keys().map(String::as_str).collect();
        names.sort_unstable();
        Some(names)
    }

    fn definition_of_inner(
        &self,
        file: &Path,
        name: &str,
        visited: &mut HashSet<PathBuf>,
    ) -> Option<DefSite> {
        let file = normalize_path(file);
        if !visited.insert(file.clone()) {
            return None;
        }
        let current = self.modules.get(&file)?;

        if let Some(local) = current.declarations.get(name) {
            return Some(local.clone());
        }

        // `pub import { name } from "..."` — follow the first recorded
        // source. Conflicting re-exports surface separately as
        // diagnostics; here we just pick a canonical destination so
        // go-to-definition lands somewhere useful.
        if let Some(sources) = current.selective_re_exports.get(name) {
            for source in sources {
                if let Some(def) = self.definition_of_inner(source, name, visited) {
                    return Some(def);
                }
            }
        }

        // `pub import "..."` — chase each wildcard re-export source.
        for source in &current.wildcard_re_export_paths {
            if let Some(def) = self.definition_of_inner(source, name, visited) {
                return Some(def);
            }
        }

        // Private selective imports.
        for import in &current.imports {
            let Some(selective_names) = &import.selective_names else {
                continue;
            };
            if !selective_names.contains(name) {
                continue;
            }
            if let Some(path) = &import.path {
                if let Some(def) = self.definition_of_inner(path, name, visited) {
                    return Some(def);
                }
            }
        }

        // Private wildcard imports.
        for import in &current.imports {
            if import.selective_names.is_some() {
                continue;
            }
            if let Some(path) = &import.path {
                if let Some(def) = self.definition_of_inner(path, name, visited) {
                    return Some(def);
                }
            }
        }

        None
    }

    fn export_definition_of_inner(
        &self,
        file: &Path,
        name: &str,
        visited: &mut HashSet<PathBuf>,
    ) -> Option<DefSite> {
        let file = normalize_path(file);
        if !visited.insert(file.clone()) {
            return None;
        }
        let current = self.modules.get(&file)?;

        if current.own_exports.contains(name) {
            if let Some(local) = current.declarations.get(name) {
                return Some(local.clone());
            }
        }
        if let Some(sources) = current.selective_re_exports.get(name) {
            for source in sources {
                if let Some(definition) = self.export_definition_of_inner(source, name, visited) {
                    return Some(definition);
                }
            }
        }
        for source in &current.wildcard_re_export_paths {
            if let Some(definition) = self.export_definition_of_inner(source, name, visited) {
                return Some(definition);
            }
        }
        None
    }

    /// Diagnostics for re-export conflicts inside `file`. Each diagnostic
    /// names the conflicting symbol and the modules that contributed it,
    /// so check-time errors can be precise.
    pub fn re_export_conflicts(&self, file: &Path) -> Vec<ReExportConflict> {
        let file = normalize_path(file);
        let Some(module) = self.modules.get(&file) else {
            return Vec::new();
        };

        // Build, for each re-exported name, the set of source modules it
        // could resolve to. Names that resolve to more than one source are
        // ambiguous and reported.
        let mut sources: HashMap<String, Vec<PathBuf>> = HashMap::new();

        for (name, srcs) in &module.selective_re_exports {
            sources
                .entry(name.clone())
                .or_default()
                .extend(srcs.iter().cloned());
        }
        for src in &module.wildcard_re_export_paths {
            let canonical = normalize_path(src);
            let Some(src_module) = self
                .modules
                .get(&canonical)
                .or_else(|| self.modules.get(src))
            else {
                continue;
            };
            for name in &src_module.exports {
                sources
                    .entry(name.clone())
                    .or_default()
                    .push(canonical.clone());
            }
        }

        // A re-export that collides with a locally exported declaration is
        // also an error: the facade module cannot expose two different
        // bindings under the same name.
        for name in &module.own_exports {
            if let Some(entry) = sources.get_mut(name) {
                entry.push(file.clone());
            }
        }

        let mut conflicts = Vec::new();
        for (name, mut srcs) in sources {
            srcs.sort();
            srcs.dedup();
            if srcs.len() > 1 {
                conflicts.push(ReExportConflict {
                    name,
                    sources: srcs,
                });
            }
        }
        conflicts.sort_by(|a, b| a.name.cmp(&b.name));
        conflicts
    }

    /// Invalid selective imports in `file`, classified as missing or private.
    ///
    /// This is the static single source of truth for the runtime's export
    /// boundary. Consumers project the typed issue into CLI or editor
    /// diagnostics without independently re-resolving names or spans.
    pub fn selective_import_issues(&self, file: &Path) -> Vec<SelectiveImportIssue> {
        let file = normalize_path(file);
        let Some(module) = self.modules.get(&file) else {
            return Vec::new();
        };

        let mut out = Vec::new();
        for import in &module.imports {
            let Some(selective) = &import.selective_names else {
                continue;
            };
            let Some(import_path) = &import.path else {
                continue;
            };
            let Some(target) = self
                .modules
                .get(import_path)
                .or_else(|| self.modules.get(&normalize_path(import_path)))
            else {
                continue;
            };
            if target.load_error.is_some() {
                continue;
            }
            for name in selective {
                let kind = if target.exports.contains(name) {
                    continue;
                } else if target.declarations.contains_key(name) {
                    SelectiveImportIssueKind::Private
                } else {
                    SelectiveImportIssueKind::Missing
                };
                out.push(SelectiveImportIssue {
                    name: name.clone(),
                    module: import.raw_path.clone(),
                    span: import.import_span,
                    kind,
                });
            }
        }
        out.sort_by(|a, b| (&a.name, &a.module, a.kind).cmp(&(&b.name, &b.module, b.kind)));
        out.dedup();
        out
    }

    /// Return the declaration kind for a name on a module's public surface.
    /// Explicit and wildcard re-exports are followed so callers can consume
    /// the same source-owned contract regardless of the import path used.
    pub fn exported_kind(&self, file: &Path, name: &str) -> Option<DefKind> {
        self.exported_kind_inner(file, name, &mut HashSet::new())
    }

    fn exported_kind_inner(
        &self,
        file: &Path,
        name: &str,
        visited: &mut HashSet<PathBuf>,
    ) -> Option<DefKind> {
        let file = normalize_path(file);
        if !visited.insert(file.clone()) {
            return None;
        }
        let result = self.modules.get(&file).and_then(|module| {
            if module.own_exports.contains(name) {
                return module
                    .declarations
                    .get(name)
                    .map(|definition| definition.kind)
                    .or_else(|| {
                        stdlib_module_from_path(&file).and_then(|stdlib_module| {
                            stdlib::builtin_reexports(stdlib_module)
                                .contains(&name)
                                .then_some(DefKind::Function)
                        })
                    });
            }
            if let Some(sources) = module.selective_re_exports.get(name) {
                for source in sources {
                    if let Some(kind) = self.exported_kind_inner(source, name, visited) {
                        return Some(kind);
                    }
                }
            }
            for source in &module.wildcard_re_export_paths {
                if let Some(kind) = self.exported_kind_inner(source, name, visited) {
                    return Some(kind);
                }
            }
            None
        });
        visited.remove(&file);
        result
    }
}

/// A duplicate or ambiguous re-export inside a single module. Reported by
/// [`ModuleGraph::re_export_conflicts`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReExportConflict {
    pub name: String,
    pub sources: Vec<PathBuf>,
}

/// Why a selective import cannot cross the target module's export boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SelectiveImportIssueKind {
    /// No declaration or transitive export has the requested name.
    Missing,
    /// The target declares the requested name without exporting it.
    Private,
}

/// A selective import rejected by the target module's public surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectiveImportIssue {
    /// The requested name.
    pub name: String,
    /// The module path exactly as written in the import statement.
    pub module: String,
    /// Span anchored to the selective import statement.
    pub span: Span,
    /// Whether the requested name is absent or private.
    pub kind: SelectiveImportIssueKind,
}

impl SelectiveImportIssue {
    /// Stable user-facing explanation shared by CLI and editor projections.
    #[must_use]
    pub fn message(&self) -> String {
        match self.kind {
            SelectiveImportIssueKind::Missing => format!(
                "imported symbol `{}` does not exist in `{}`",
                self.name, self.module
            ),
            SelectiveImportIssueKind::Private => format!(
                "imported symbol `{}` is not exported by `{}` — it is defined there but not `pub`",
                self.name, self.module
            ),
        }
    }

    /// Stable repair guidance shared by CLI and editor projections.
    #[must_use]
    pub fn help(&self) -> String {
        match self.kind {
            SelectiveImportIssueKind::Missing => format!(
                "update the import to a symbol exported by `{}`",
                self.module
            ),
            SelectiveImportIssueKind::Private => {
                format!(
                    "mark `{}` as `pub` in `{}` to export it",
                    self.name, self.module
                )
            }
        }
    }
}

fn load_module(
    path: &Path,
    package_snapshots: &[PackageSnapshot],
    source_overrides: Option<&HashMap<PathBuf, String>>,
) -> (ModuleInfo, Option<ParsedModuleSource>) {
    let source = source_overrides
        .and_then(|overrides| overrides.get(&normalize_path(path)).cloned())
        .or_else(|| read_module_source(path));
    let Some(source) = source else {
        return (ModuleInfo::default(), None);
    };
    let mut lexer = harn_lexer::Lexer::new(&source);
    let tokens = match lexer.tokenize() {
        Ok(tokens) => tokens,
        Err(error) => {
            let module = ModuleInfo {
                load_error: Some(ModuleLoadError {
                    message: error.to_string(),
                    span: error.span(),
                }),
                ..ModuleInfo::default()
            };
            return (module, None);
        }
    };
    let mut parser = Parser::new(tokens);
    let program = match parser.parse() {
        Ok(program) => program,
        Err(error) => {
            let module = ModuleInfo {
                load_error: Some(ModuleLoadError {
                    message: error.to_string(),
                    span: error.span(),
                }),
                ..ModuleInfo::default()
            };
            return (module, None);
        }
    };

    let mut module = ModuleInfo::default();
    for node in &program {
        collect_module_info(path, node, &mut module, package_snapshots);
        collect_type_declarations(node, &mut module.type_declarations);
        collect_callable_declarations(node, &mut module.callable_declarations);
    }
    if let Some(stdlib_module) = stdlib_module_from_path(path) {
        module.own_exports.extend(
            stdlib::builtin_reexports(stdlib_module)
                .iter()
                .map(|name| (*name).to_string()),
        );
    }
    // Seed the transitive `exports` set from local exports plus selective
    // re-export names. Wildcard re-exports are folded in by
    // [`resolve_re_exports`] after every module has been loaded.
    module.exports.extend(module.own_exports.iter().cloned());
    module
        .exports
        .extend(module.selective_re_exports.keys().cloned());
    let parsed = ParsedModuleSource { source, program };
    (module, Some(parsed))
}

/// Extract the stdlib module name when `path` is a `<std>/<name>`
/// virtual path, otherwise `None`.
fn stdlib_module_from_path(path: &Path) -> Option<&str> {
    let s = path.to_str()?;
    s.strip_prefix("<std>/")
}

fn collect_module_info(
    file: &Path,
    snode: &SNode,
    module: &mut ModuleInfo,
    package_snapshots: &[PackageSnapshot],
) {
    if let Node::AttributedDecl { inner, .. } = &snode.node {
        collect_module_info(file, inner, module, package_snapshots);
        return;
    }

    for public in public_declarations(snode) {
        module.own_exports.insert(public.name);
    }

    match &snode.node {
        Node::FnDecl { name, params, .. } => {
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Function),
            );
            for param_name in params.iter().map(|param| param.name.clone()) {
                module.declarations.insert(
                    param_name.clone(),
                    decl_site(file, snode.span, &param_name, DefKind::Parameter),
                );
            }
        }
        Node::Pipeline { name, .. } => {
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Pipeline),
            );
        }
        Node::ToolDecl { name, .. } => {
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Tool),
            );
        }
        Node::SkillDecl { name, .. } => {
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Skill),
            );
        }
        Node::EvalPackDecl { binding_name, .. } => {
            module.declarations.insert(
                binding_name.clone(),
                decl_site(file, snode.span, binding_name, DefKind::EvalPack),
            );
        }
        Node::StructDecl { name, .. } => {
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Struct),
            );
        }
        Node::EnumDecl { name, .. } => {
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Enum),
            );
        }
        Node::InterfaceDecl { name, .. } => {
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Interface),
            );
        }
        Node::TypeDecl { name, .. } => {
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Type),
            );
        }
        Node::LetBinding { pattern, .. } | Node::ConstBinding { pattern, .. } => {
            for name in pattern_names(pattern) {
                module.declarations.insert(
                    name.clone(),
                    decl_site(file, snode.span, &name, DefKind::Variable),
                );
            }
        }
        Node::ImportDecl { path, is_pub } => {
            let import_path = resolve_import_path_with_snapshots(file, path, package_snapshots);
            if import_path.is_none() {
                module.has_unresolved_wildcard_import = true;
            }
            if *is_pub {
                if let Some(resolved) = &import_path {
                    module
                        .wildcard_re_export_paths
                        .push(normalize_path(resolved));
                }
            }
            module.imports.push(ImportRef {
                raw_path: path.clone(),
                path: import_path,
                selective_names: None,
                import_span: snode.span,
            });
        }
        Node::SelectiveImport {
            names,
            path,
            is_pub,
        } => {
            let import_path = resolve_import_path_with_snapshots(file, path, package_snapshots);
            if import_path.is_none() {
                module.has_unresolved_selective_import = true;
            }
            if *is_pub {
                if let Some(resolved) = &import_path {
                    let canonical = normalize_path(resolved);
                    for name in names {
                        module
                            .selective_re_exports
                            .entry(name.clone())
                            .or_default()
                            .push(canonical.clone());
                    }
                }
            }
            let names: HashSet<String> = names.iter().cloned().collect();
            module.selective_import_names.extend(names.iter().cloned());
            module.imports.push(ImportRef {
                raw_path: path.clone(),
                path: import_path,
                selective_names: Some(names),
                import_span: snode.span,
            });
        }
        _ => {}
    }
}

fn collect_type_declarations(snode: &SNode, decls: &mut Vec<SNode>) {
    match &snode.node {
        Node::TypeDecl { .. }
        | Node::StructDecl { .. }
        | Node::EnumDecl { .. }
        | Node::InterfaceDecl { .. } => decls.push(snode.clone()),
        Node::AttributedDecl { inner, .. } => collect_type_declarations(inner, decls),
        _ => {}
    }
}

fn collect_callable_declarations(snode: &SNode, decls: &mut Vec<SNode>) {
    match &snode.node {
        Node::FnDecl { .. } | Node::Pipeline { .. } | Node::ToolDecl { .. } => {
            decls.push(snode.clone());
        }
        Node::AttributedDecl { inner, .. } => collect_callable_declarations(inner, decls),
        _ => {}
    }
}

fn type_decl_name(snode: &SNode) -> Option<&str> {
    match &snode.node {
        Node::TypeDecl { name, .. }
        | Node::StructDecl { name, .. }
        | Node::EnumDecl { name, .. }
        | Node::InterfaceDecl { name, .. } => Some(name.as_str()),
        _ => None,
    }
}

fn callable_decl_name(snode: &SNode) -> Option<&str> {
    match &snode.node {
        Node::FnDecl { name, .. } | Node::Pipeline { name, .. } | Node::ToolDecl { name, .. } => {
            Some(name.as_str())
        }
        Node::AttributedDecl { inner, .. } => callable_decl_name(inner),
        _ => None,
    }
}

fn decl_site(file: &Path, span: Span, name: &str, kind: DefKind) -> DefSite {
    DefSite {
        name: name.to_string(),
        file: file.to_path_buf(),
        kind,
        span,
    }
}

fn pattern_names(pattern: &BindingPattern) -> Vec<String> {
    match pattern {
        BindingPattern::Identifier(name) => vec![name.clone()],
        BindingPattern::Dict(fields) => fields
            .iter()
            .filter_map(|field| field.alias.as_ref().or(Some(&field.key)).cloned())
            .collect(),
        BindingPattern::List(elements) => elements
            .iter()
            .map(|element| element.name.clone())
            .collect(),
        BindingPattern::Pair(a, b) => vec![a.clone(), b.clone()],
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    canonical_path(path)
}

/// Canonicalize `path`, memoized process-wide.
///
/// Module-graph construction and every per-file graph query canonicalize
/// paths to dedupe import-edge spellings, and the check preflight scan
/// canonicalizes each visited module per checked file. `Path::canonicalize`
/// resolves every component through the kernel, so a whole-tree `harn check`
/// used to spend the bulk of its wall clock in path-resolution syscalls
/// (`getattrlist` dominated system time). One positive-result memo removes
/// the `O(files x import closure)` repetition; failed canonicalizations are
/// not memoized (mirroring the bytecode cache's `canonicalize_cached`) so a
/// file that appears later still resolves correctly in long-lived processes.
/// `<std>/` virtual paths pass through untouched.
pub fn canonical_path(path: &Path) -> PathBuf {
    use std::sync::OnceLock;
    if stdlib_module_from_path(path).is_some() {
        return path.to_path_buf();
    }
    static MEMO: OnceLock<std::sync::Mutex<HashMap<PathBuf, PathBuf>>> = OnceLock::new();
    let memo = MEMO.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    if let Some(hit) = memo
        .lock()
        .expect("canonical path memo lock poisoned")
        .get(path)
        .cloned()
    {
        return hit;
    }
    match path.canonicalize() {
        Ok(canonical) => {
            memo.lock()
                .expect("canonical path memo lock poisoned")
                .insert(path.to_path_buf(), canonical.clone());
            canonical
        }
        Err(_) => path.to_path_buf(),
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
