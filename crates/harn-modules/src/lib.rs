use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::package_imports::{acquire_package_snapshots, resolve_import_path_with_snapshots};
use crate::package_snapshot::PackageSnapshot;
use harn_lexer::Span;
use harn_parser::{BindingPattern, Node, Parser, SNode};

pub mod asset_paths;
pub mod fingerprint;
mod package_imports;
pub mod package_snapshot;
pub mod personas;
mod stdlib;

pub use package_imports::resolve_import_path;

/// Kind of symbol that can be exported by a module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefKind {
    Function,
    Pipeline,
    Tool,
    Skill,
    Struct,
    Enum,
    Interface,
    Type,
    Variable,
    Parameter,
}

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
    /// `pub struct`, etc., or every `fn` under the no-`pub fn` fallback.
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
    build_inner(files, None).graph
}

/// Build a module graph while retaining parsed sources for the seed files.
///
/// Imported-only modules still participate in the graph, but their ASTs are
/// dropped after graph extraction so callers do not pay extra peak memory for
/// parsed sources they will not reuse.
pub fn build_with_parsed_sources(files: &[PathBuf]) -> ModuleGraphBuild {
    let parsed_source_targets = files.iter().map(|file| normalize_path(file)).collect();
    build_inner(files, Some(&parsed_source_targets))
}

fn build_inner(
    files: &[PathBuf],
    parsed_source_targets: Option<&HashSet<PathBuf>>,
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
        let loaded = load_wave(&wave, &package_snapshots);
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
            .map(|path| load_module(path, package_snapshots))
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
                            local.push((index, load_module(path, package_snapshots)));
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
            // privacy is still enforced by `non_exported_selective_imports`
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

    /// Selective imports in `file` that name a symbol the target module
    /// declares but does not export — a non-`pub` function in a module that
    /// has opted into explicit exports by marking at least one function `pub`.
    ///
    /// Such names are private: importing them by name is no more valid than a
    /// wildcard import reaching them, and matches the strict visibility of
    /// TypeScript, Rust, and Go. This is the single source of truth for that
    /// determination — the CLI maps the result onto import spans and emits
    /// `HARN-IMP-002`, and the runtime loader enforces the same rule. A module
    /// that marks nothing `pub` exports nothing, so selectively importing any
    /// name it declares is flagged.
    pub fn non_exported_selective_imports(&self, file: &Path) -> Vec<NonExportedImport> {
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
            for name in selective {
                // Declared in the target but absent from its export surface
                // (and not a re-export, which lives in `exports`, not
                // `declarations`).
                if target.declarations.contains_key(name) && !target.exports.contains(name) {
                    out.push(NonExportedImport {
                        name: name.clone(),
                        module: import.raw_path.clone(),
                    });
                }
            }
        }
        out.sort_by(|a, b| (&a.name, &a.module).cmp(&(&b.name, &b.module)));
        out.dedup();
        out
    }
}

/// A duplicate or ambiguous re-export inside a single module. Reported by
/// [`ModuleGraph::re_export_conflicts`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReExportConflict {
    pub name: String,
    pub sources: Vec<PathBuf>,
}

/// A selective import of a name the target module declares but does not
/// export. Reported by [`ModuleGraph::non_exported_selective_imports`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonExportedImport {
    /// The non-exported name the import requested.
    pub name: String,
    /// The module path exactly as written in the import statement.
    pub module: String,
}

fn load_module(
    path: &Path,
    package_snapshots: &[PackageSnapshot],
) -> (ModuleInfo, Option<ParsedModuleSource>) {
    let Some(source) = read_module_source(path) else {
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
    match &snode.node {
        Node::FnDecl {
            name,
            params,
            is_pub,
            ..
        } => {
            if *is_pub {
                module.own_exports.insert(name.clone());
            }
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
        Node::Pipeline { name, is_pub, .. } => {
            if *is_pub {
                module.own_exports.insert(name.clone());
            }
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Pipeline),
            );
        }
        Node::ToolDecl { name, is_pub, .. } => {
            if *is_pub {
                module.own_exports.insert(name.clone());
            }
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Tool),
            );
        }
        Node::SkillDecl { name, is_pub, .. } => {
            if *is_pub {
                module.own_exports.insert(name.clone());
            }
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Skill),
            );
        }
        Node::StructDecl { name, is_pub, .. } => {
            if *is_pub {
                module.own_exports.insert(name.clone());
            }
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Struct),
            );
        }
        Node::EnumDecl { name, is_pub, .. } => {
            if *is_pub {
                module.own_exports.insert(name.clone());
            }
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Enum),
            );
        }
        Node::InterfaceDecl { name, .. } => {
            module.own_exports.insert(name.clone());
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Interface),
            );
        }
        Node::TypeDecl { name, is_pub, .. } => {
            if *is_pub {
                module.own_exports.insert(name.clone());
            }
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Type),
            );
        }
        Node::LetBinding {
            pattern, is_pub, ..
        }
        | Node::ConstBinding {
            pattern, is_pub, ..
        } => {
            for name in pattern_names(pattern) {
                // A top-level `pub const`/`pub let` exports its (identifier)
                // binding as part of the module's public value surface, on the
                // same footing as `pub fn`.
                if *is_pub {
                    module.own_exports.insert(name.clone());
                }
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
        Node::AttributedDecl { inner, .. } => {
            collect_module_info(file, inner, module, package_snapshots);
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
mod tests {
    use super::*;
    use crate::package_snapshot::probe_counter;
    use std::fs;

    fn write_file(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, contents).unwrap();
        path
    }

    fn package_fixture(root: &Path) -> PathBuf {
        use crate::package_snapshot::{
            generation_root, package_current_path, package_publication_lock_path,
            PackageGenerationManifest, PackageGenerationPointer, GENERATION_LEASE_FILE,
            GENERATION_LOCK_FILE, GENERATION_MANIFEST_FILE, GENERATION_PACKAGES_DIR,
        };

        let generation = "generation-test";
        let generation_root = generation_root(root, generation);
        let packages_root = generation_root.join(GENERATION_PACKAGES_DIR);
        fs::create_dir_all(&packages_root).unwrap();
        fs::write(generation_root.join(GENERATION_LOCK_FILE), "version = 4\n").unwrap();
        fs::write(generation_root.join(GENERATION_LEASE_FILE), []).unwrap();
        let manifest = PackageGenerationManifest::new(
            generation,
            crate::package_snapshot::package_lock_digest(b"version = 4\n"),
        )
        .unwrap();
        fs::write(
            generation_root.join(GENERATION_MANIFEST_FILE),
            toml::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let pointer = PackageGenerationPointer::new(generation).unwrap();
        fs::write(
            package_current_path(root),
            toml::to_string_pretty(&pointer).unwrap(),
        )
        .unwrap();
        fs::File::create(package_publication_lock_path(root)).unwrap();
        packages_root
    }

    #[test]
    fn wave_parallel_build_matches_serial_semantics() {
        // Seed enough files to cross MIN_PARALLEL_WAVE so `load_wave` takes
        // the threaded path, and verify the graph resolves exactly as the
        // serial walk always did: every seed sees the shared module's export
        // and the shared module knows all its importers.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "shared.harn", "pub fn shared_fn() { 1 }\n");
        let seeds: Vec<PathBuf> = (0..12)
            .map(|i| {
                write_file(
                    root,
                    &format!("mod{i}.harn"),
                    &format!(
                        "import {{ shared_fn }} from \"./shared\"\npub fn f{i}() {{ shared_fn() }}\n"
                    ),
                )
            })
            .collect();

        let graph = build(&seeds);
        for seed in &seeds {
            let names = graph
                .imported_names_for_file(seed)
                .expect("seed imports should resolve");
            assert!(names.contains("shared_fn"));
        }
        let importers = graph.importers_of(&root.join("shared.harn"));
        assert_eq!(importers.len(), seeds.len());
    }

    #[test]
    fn pub_const_and_let_are_exported() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(
            root,
            "consts.harn",
            "pub const MAX = 3\npub let SEED = 7\nconst PRIVATE = 9\n",
        );
        let consumer = write_file(
            root,
            "use.harn",
            "import { MAX, SEED } from \"./consts\"\nMAX\n",
        );

        let graph = build(std::slice::from_ref(&consumer));
        let names = graph
            .imported_names_for_file(&consumer)
            .expect("imports resolve");
        assert!(names.contains("MAX"), "pub const should be importable");
        assert!(names.contains("SEED"), "pub let should be importable");
        // A private const stays out of the export surface.
        let consts_exports = graph.exports_for_module(&root.join("consts.harn"));
        assert!(consts_exports.contains(&"MAX".to_string()));
        assert!(consts_exports.contains(&"SEED".to_string()));
        assert!(!consts_exports.contains(&"PRIVATE".to_string()));
    }

    #[test]
    fn import_compile_failures_point_at_broken_module() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // A syntax error makes the whole library fail to parse.
        write_file(
            root,
            "lib.harn",
            "pub fn ok() { 1 }\npub fn broken( {\n  2\n}\n",
        );
        let consumer = write_file(
            root,
            "main.harn",
            "import { ok } from \"./lib\"\npipeline test(task) { ok() }\n",
        );

        let graph = build(std::slice::from_ref(&consumer));
        let failures = graph.import_compile_failures(&consumer);
        assert_eq!(failures.len(), 1, "the broken import should be reported");
        assert_eq!(failures[0].import_raw_path, "./lib");
        assert!(
            failures[0]
                .module_path
                .to_string_lossy()
                .ends_with("lib.harn"),
            "failure must name the imported module, not the consumer"
        );

        // The consumer's undefined-name check falls back to conservative
        // `None` rather than flagging `ok` as undefined at its call site.
        assert!(
            graph.imported_names_for_file(&consumer).is_none(),
            "a broken import target should suppress the call-site undefined check"
        );
    }

    #[test]
    fn importers_of_finds_direct_dependents() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let leaf = write_file(root, "leaf.harn", "pub fn leaf() { 1 }\n");
        write_file(root, "a.harn", "import \"./leaf\"\nleaf()\n");
        write_file(root, "b.harn", "import { leaf } from \"./leaf\"\nleaf()\n");
        let entry = write_file(root, "entry.harn", "import \"./a\"\nimport \"./b\"\n");

        let graph = build(std::slice::from_ref(&entry));
        let importers = graph.importers_of(&leaf);
        let names: Vec<String> = importers
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"a.harn".to_string()));
        assert!(names.contains(&"b.harn".to_string()));
        assert!(!names.contains(&"entry.harn".to_string()));
    }

    #[test]
    fn recursive_build_loads_transitively_imported_modules() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "leaf.harn", "pub fn leaf_fn() { 1 }\n");
        write_file(
            root,
            "mid.harn",
            "import \"./leaf\"\npub fn mid_fn() { leaf_fn() }\n",
        );
        let entry = write_file(root, "entry.harn", "import \"./mid\"\nmid_fn()\n");

        let graph = build(std::slice::from_ref(&entry));
        let imported = graph
            .imported_names_for_file(&entry)
            .expect("entry imports should resolve");
        // Wildcard import of mid exposes mid_fn (pub) but not leaf_fn.
        assert!(imported.contains("mid_fn"));
        assert!(!imported.contains("leaf_fn"));

        // The transitively loaded module is known to the graph even though
        // the seed only included entry.harn.
        let leaf_path = root.join("leaf.harn");
        assert!(graph.definition_of(&leaf_path, "leaf_fn").is_some());
    }

    #[test]
    fn imported_names_returns_none_when_import_unresolved() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let entry = write_file(root, "entry.harn", "import \"./does_not_exist\"\n");

        let graph = build(std::slice::from_ref(&entry));
        assert!(graph.imported_names_for_file(&entry).is_none());
    }

    #[test]
    fn selective_imports_contribute_only_requested_names() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "util.harn", "pub fn a() { 1 }\npub fn b() { 2 }\n");
        let entry = write_file(root, "entry.harn", "import { a } from \"./util\"\n");

        let graph = build(std::slice::from_ref(&entry));
        let imported = graph
            .imported_names_for_file(&entry)
            .expect("entry imports should resolve");
        assert!(imported.contains("a"));
        assert!(!imported.contains("b"));
    }

    #[test]
    fn non_exported_selective_import_is_flagged_when_module_has_pub() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "lib.harn", "pub fn api() { 1 }\nfn helper() { 2 }\n");
        let entry = write_file(root, "entry.harn", "import { helper } from \"./lib\"\n");

        let graph = build(std::slice::from_ref(&entry));
        let offenders = graph.non_exported_selective_imports(&entry);
        assert_eq!(offenders.len(), 1);
        assert_eq!(offenders[0].name, "helper");
        assert_eq!(offenders[0].module, "./lib");

        // Importing the `pub` name is fine.
        let entry_ok = write_file(root, "entry_ok.harn", "import { api } from \"./lib\"\n");
        let graph_ok = build(std::slice::from_ref(&entry_ok));
        assert!(graph_ok
            .non_exported_selective_imports(&entry_ok)
            .is_empty());
    }

    #[test]
    fn selective_import_from_zero_pub_module_is_flagged() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // A module with no `pub` markers exports nothing — Harn has no
        // "public-by-default" fallback — so selectively importing any of its
        // functions is flagged just like importing a private name.
        write_file(root, "util.harn", "fn a() { 1 }\nfn b() { 2 }\n");
        let entry = write_file(root, "entry.harn", "import { a } from \"./util\"\n");

        let graph = build(std::slice::from_ref(&entry));
        let offenders = graph.non_exported_selective_imports(&entry);
        assert_eq!(offenders.len(), 1);
        assert_eq!(offenders[0].name, "a");
        assert_eq!(offenders[0].module, "./util");
    }

    #[test]
    fn stdlib_imports_resolve_to_embedded_sources() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let entry = write_file(root, "entry.harn", "import \"std/math\"\nclamp(5, 0, 10)\n");

        let graph = build(std::slice::from_ref(&entry));
        let imported = graph
            .imported_names_for_file(&entry)
            .expect("std/math should resolve");
        // `clamp` is defined in stdlib_math.harn as `pub fn clamp(...)`.
        assert!(imported.contains("clamp"));
    }

    #[test]
    fn stdlib_internal_imports_resolve_without_leaking_to_callers() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let entry = write_file(
            root,
            "entry.harn",
            "import { process_run } from \"std/runtime\"\nprocess_run([\"echo\", \"ok\"])\n",
        );

        let graph = build(std::slice::from_ref(&entry));
        let entry_imports = graph
            .imported_names_for_file(&entry)
            .expect("std/runtime should resolve");
        assert!(entry_imports.contains("process_run"));
        assert!(
            !entry_imports.contains("filter_nil"),
            "private std/runtime dependency leaked to caller"
        );

        let runtime_path = stdlib::stdlib_virtual_path("runtime");
        let runtime_imports = graph
            .imported_names_for_file(&runtime_path)
            .expect("std/runtime internal imports should resolve");
        assert!(runtime_imports.contains("filter_nil"));
    }

    #[test]
    fn runtime_stdlib_import_surface_resolves_to_embedded_sources() {
        let tmp = tempfile::tempdir().unwrap();
        let entry_path = write_file(tmp.path(), "entry.harn", "");

        for source in harn_stdlib::STDLIB_SOURCES {
            let import_path = format!("std/{}", source.module);
            assert!(
                resolve_import_path(&entry_path, &import_path).is_some(),
                "{import_path} should resolve in the module graph"
            );
        }
    }

    #[test]
    fn stdlib_imports_expose_type_declarations() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let entry = write_file(
            root,
            "entry.harn",
            "import \"std/triggers\"\nlet provider = \"github\"\n",
        );

        let graph = build(std::slice::from_ref(&entry));
        let decls = graph
            .imported_type_declarations_for_file(&entry)
            .expect("std/triggers type declarations should resolve");
        let names: HashSet<String> = decls
            .iter()
            .filter_map(type_decl_name)
            .map(ToString::to_string)
            .collect();
        assert!(names.contains("TriggerEvent"));
        assert!(names.contains("ProviderPayload"));
        assert!(names.contains("SignatureStatus"));
    }

    #[test]
    fn stdlib_imports_expose_callable_declarations() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let entry = write_file(
            root,
            "entry.harn",
            "import { select_from } from \"std/tui\"\nlet item = \"alpha\"\n",
        );

        let graph = build(std::slice::from_ref(&entry));
        let decls = graph
            .imported_callable_declarations_for_file(&entry)
            .expect("std/tui callable declarations should resolve");
        let names: HashSet<String> = decls
            .iter()
            .filter_map(callable_decl_name)
            .map(ToString::to_string)
            .collect();
        assert!(names.contains("select_from"));
    }

    #[test]
    fn stdlib_llm_catalog_exposes_routing_routes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let entry = write_file(
            root,
            "entry.harn",
            "import { routing_routes } from \"std/llm/catalog\"\nrouting_routes()\n",
        );

        let graph = build(std::slice::from_ref(&entry));
        let imported = graph
            .imported_names_for_file(&entry)
            .expect("std/llm/catalog should resolve");
        assert!(imported.contains("routing_routes"));
        let decls = graph
            .imported_callable_declarations_for_file(&entry)
            .expect("std/llm/catalog callable declarations should resolve");
        let names: HashSet<String> = decls
            .iter()
            .filter_map(callable_decl_name)
            .map(ToString::to_string)
            .collect();
        assert!(names.contains("routing_routes"));
    }

    #[test]
    fn package_export_map_resolves_declared_module() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let packages_root = package_fixture(root);
        let packages = packages_root.join("acme/runtime");
        fs::create_dir_all(&packages).unwrap();
        fs::write(
            packages_root.join("acme/harn.toml"),
            "[exports]\ncapabilities = \"runtime/capabilities.harn\"\n",
        )
        .unwrap();
        fs::write(
            packages.join("capabilities.harn"),
            "pub fn exported_capability() { 1 }\n",
        )
        .unwrap();
        let entry = write_file(
            root,
            "entry.harn",
            "import \"acme/capabilities\"\nexported_capability()\n",
        );

        let graph = build(std::slice::from_ref(&entry));
        let imported = graph
            .imported_names_for_file(&entry)
            .expect("package export should resolve");
        assert!(imported.contains("exported_capability"));
    }

    /// Only a package import can be answered by a package, so only a package
    /// import may pay to find one.
    ///
    /// harn#4657 hoisted `PackageSnapshot::acquire_nearest` above the stdlib and
    /// relative-path checks, so every `std/...` and every sibling import walked
    /// its ancestors stat-ing for a package pointer, then opened, flocked and
    /// parsed it — and discarded the snapshot unused. Those are the two
    /// overwhelmingly common import shapes. It cost ~5x per-test module setup,
    /// 1.8x on a downstream CI critical path, and it was invisible for three
    /// releases because the wasted work changes nothing except wall time
    /// (harn#4815).
    #[test]
    fn stdlib_and_relative_imports_never_probe_for_a_package() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // A real installed package, so a probe would find something and the
        // test cannot pass merely because there is nothing to look for.
        package_fixture(root);
        write_file(root, "sibling.harn", "pub fn helper() { 1 }\n");
        let entry = write_file(root, "entry.harn", "");

        let (resolved, probes) =
            probe_counter::count_probes(|| resolve_import_path(&entry, "std/testing"));
        assert!(resolved.is_some(), "std/testing must still resolve");
        assert_eq!(
            probes, 0,
            "a std/ import probed the filesystem for a package"
        );

        let (resolved, probes) =
            probe_counter::count_probes(|| resolve_import_path(&entry, "./sibling"));
        assert!(resolved.is_some(), "a relative sibling must still resolve");
        assert_eq!(
            probes, 0,
            "a relative import probed the filesystem for a package"
        );
    }

    /// The counter above only means something if a real package import still
    /// probes — otherwise the assertions would hold even with resolution
    /// removed entirely.
    #[test]
    fn a_package_import_still_acquires_a_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let packages_root = package_fixture(root);
        fs::create_dir_all(packages_root.join("acme")).unwrap();
        fs::write(
            packages_root.join("acme/capabilities.harn"),
            "pub fn exported_capability() { 1 }\n",
        )
        .unwrap();
        let entry = write_file(root, "entry.harn", "");

        let (resolved, probes) =
            probe_counter::count_probes(|| resolve_import_path(&entry, "acme/capabilities"));
        assert!(resolved.is_some(), "a package import must still resolve");
        assert_eq!(
            probes, 1,
            "a package import must acquire exactly one snapshot"
        );
    }

    /// A `std/` import that names no real module resolves to nothing and must
    /// not fall through to package resolution — otherwise a package could
    /// shadow the standard library namespace.
    #[test]
    fn an_unknown_stdlib_module_does_not_fall_through_to_packages() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let packages_root = package_fixture(root);
        fs::create_dir_all(packages_root.join("std")).unwrap();
        fs::write(
            packages_root.join("std/not_a_real_module.harn"),
            "pub fn impostor() { 1 }\n",
        )
        .unwrap();
        let entry = write_file(root, "entry.harn", "");

        let (resolved, probes) =
            probe_counter::count_probes(|| resolve_import_path(&entry, "std/not_a_real_module"));
        assert!(
            resolved.is_none(),
            "a package resolved a std/ import and shadowed the stdlib namespace"
        );
        assert_eq!(probes, 0, "an unknown std/ import probed for a package");
    }

    #[test]
    fn package_direct_import_cannot_escape_packages_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(package_fixture(root).join("acme")).unwrap();
        fs::write(root.join("secret.harn"), "pub fn leaked() { 1 }\n").unwrap();
        let entry = write_file(root, "entry.harn", "");

        let resolved = resolve_import_path(&entry, "acme/../../secret");
        assert!(resolved.is_none(), "package import escaped package root");
    }

    #[test]
    fn package_export_map_cannot_escape_package_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let packages_root = package_fixture(root);
        fs::create_dir_all(packages_root.join("acme")).unwrap();
        fs::write(root.join("secret.harn"), "pub fn leaked() { 1 }\n").unwrap();
        fs::write(
            packages_root.join("acme/harn.toml"),
            "[exports]\nleak = \"../../secret.harn\"\n",
        )
        .unwrap();
        let entry = write_file(root, "entry.harn", "");

        let resolved = resolve_import_path(&entry, "acme/leak");
        assert!(resolved.is_none(), "package export escaped package root");
    }

    #[test]
    fn package_export_map_allows_symlinked_path_dependencies() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let source = root.join("source-package");
        fs::create_dir_all(source.join("runtime")).unwrap();
        fs::write(
            source.join("harn.toml"),
            "[exports]\ncapabilities = \"runtime/capabilities.harn\"\n",
        )
        .unwrap();
        fs::write(
            source.join("runtime/capabilities.harn"),
            "pub fn exported_capability() { 1 }\n",
        )
        .unwrap();
        let packages_root = package_fixture(root);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&source, packages_root.join("acme")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&source, packages_root.join("acme")).unwrap();
        let entry = write_file(root, "entry.harn", "");

        let resolved = resolve_import_path(&entry, "acme/capabilities")
            .expect("symlinked package export should resolve");
        assert!(resolved.ends_with("runtime/capabilities.harn"));
    }

    #[test]
    fn package_imports_resolve_from_nested_package_module() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join(".git")).unwrap();
        let packages_root = package_fixture(root);
        fs::create_dir_all(packages_root.join("acme")).unwrap();
        fs::create_dir_all(packages_root.join("shared")).unwrap();
        fs::write(
            packages_root.join("shared/lib.harn"),
            "pub fn shared_helper() { 1 }\n",
        )
        .unwrap();
        fs::write(
            packages_root.join("acme/lib.harn"),
            "import \"shared\"\npub fn use_shared() { shared_helper() }\n",
        )
        .unwrap();
        let entry = write_file(root, "entry.harn", "import \"acme\"\nuse_shared()\n");

        let graph = build(std::slice::from_ref(&entry));
        let imported = graph
            .imported_names_for_file(&entry)
            .expect("nested package import should resolve");
        assert!(imported.contains("use_shared"));
        let acme_path = packages_root.join("acme/lib.harn");
        let acme_imports = graph
            .imported_names_for_file(&acme_path)
            .expect("package module imports should resolve");
        assert!(acme_imports.contains("shared_helper"));
    }

    #[test]
    fn unknown_stdlib_import_is_unresolved() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let entry = write_file(root, "entry.harn", "import \"std/does_not_exist\"\n");

        let graph = build(std::slice::from_ref(&entry));
        assert!(
            graph.imported_names_for_file(&entry).is_none(),
            "unknown std module should fail resolution and disable strict check"
        );
    }

    #[test]
    fn import_cycles_do_not_loop_forever() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "a.harn", "import \"./b\"\npub fn a_fn() { 1 }\n");
        write_file(root, "b.harn", "import \"./a\"\npub fn b_fn() { 1 }\n");
        let entry = root.join("a.harn");

        // Just ensuring this terminates and yields sensible names.
        let graph = build(std::slice::from_ref(&entry));
        let imported = graph
            .imported_names_for_file(&entry)
            .expect("cyclic imports still resolve to known exports");
        assert!(imported.contains("b_fn"));
    }

    #[test]
    fn pub_import_selective_re_exports_named_symbols() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(
            root,
            "src.harn",
            "pub fn alpha() { 1 }\npub fn beta() { 2 }\n",
        );
        write_file(root, "facade.harn", "pub import { alpha } from \"./src\"\n");
        let entry = write_file(root, "entry.harn", "import \"./facade\"\nalpha()\n");

        let graph = build(std::slice::from_ref(&entry));
        let imported = graph
            .imported_names_for_file(&entry)
            .expect("entry should resolve");
        assert!(imported.contains("alpha"), "selective re-export missing");
        assert!(
            !imported.contains("beta"),
            "non-listed name leaked through facade"
        );

        let facade_path = root.join("facade.harn");
        let def = graph
            .definition_of(&facade_path, "alpha")
            .expect("definition_of should chase re-export");
        assert!(def.file.ends_with("src.harn"));
    }

    #[test]
    fn pub_import_wildcard_re_exports_full_surface() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(
            root,
            "src.harn",
            "pub fn alpha() { 1 }\npub fn beta() { 2 }\n",
        );
        write_file(root, "facade.harn", "pub import \"./src\"\n");
        let entry = write_file(root, "entry.harn", "import \"./facade\"\nalpha()\n");

        let graph = build(std::slice::from_ref(&entry));
        let imported = graph
            .imported_names_for_file(&entry)
            .expect("entry should resolve");
        assert!(imported.contains("alpha"));
        assert!(imported.contains("beta"));
    }

    #[test]
    fn pub_import_chain_resolves_definition_to_origin() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "inner.harn", "pub fn deep() { 1 }\n");
        write_file(
            root,
            "middle.harn",
            "pub import { deep } from \"./inner\"\n",
        );
        write_file(
            root,
            "outer.harn",
            "pub import { deep } from \"./middle\"\n",
        );
        let entry = write_file(
            root,
            "entry.harn",
            "import { deep } from \"./outer\"\ndeep()\n",
        );

        let graph = build(std::slice::from_ref(&entry));
        let def = graph
            .definition_of(&entry, "deep")
            .expect("definition_of should follow chain");
        assert!(def.file.ends_with("inner.harn"));

        let imported = graph
            .imported_names_for_file(&entry)
            .expect("entry should resolve");
        assert!(imported.contains("deep"));
    }

    #[test]
    fn duplicate_pub_import_reports_re_export_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "a.harn", "pub fn shared() { 1 }\n");
        write_file(root, "b.harn", "pub fn shared() { 2 }\n");
        let facade = write_file(
            root,
            "facade.harn",
            "pub import { shared } from \"./a\"\npub import { shared } from \"./b\"\n",
        );

        let graph = build(std::slice::from_ref(&facade));
        let conflicts = graph.re_export_conflicts(&facade);
        assert_eq!(
            conflicts.len(),
            1,
            "expected exactly one re-export conflict, got {conflicts:?}"
        );
        assert_eq!(conflicts[0].name, "shared");
        assert_eq!(conflicts[0].sources.len(), 2);
    }

    #[test]
    fn cross_directory_cycle_does_not_explode_module_count() {
        // Regression: two files in sibling directories that import each
        // other produced a fresh path spelling on every round-trip
        // (`../runtime/../context/../runtime/...`), and `build()`'s
        // `seen` set deduped on the raw spelling rather than the
        // canonical path. The walk only terminated when `PATH_MAX` was
        // hit — 1024 on macOS, 4096 on Linux — so Linux re-parsed the
        // same pair thousands of times until it ran out of memory.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let context = root.join("context");
        let runtime = root.join("runtime");
        fs::create_dir_all(&context).unwrap();
        fs::create_dir_all(&runtime).unwrap();
        write_file(
            &context,
            "a.harn",
            "import \"../runtime/b\"\npub fn a_fn() { 1 }\n",
        );
        write_file(
            &runtime,
            "b.harn",
            "import \"../context/a\"\npub fn b_fn() { 1 }\n",
        );
        let entry = context.join("a.harn");

        let graph = build(std::slice::from_ref(&entry));
        // The graph should contain exactly the two real files, keyed by
        // their canonical paths. Pre-fix this was thousands of entries.
        assert_eq!(
            graph.modules.len(),
            2,
            "cross-directory cycle loaded {} modules, expected 2",
            graph.modules.len()
        );
        let imported = graph
            .imported_names_for_file(&entry)
            .expect("cyclic imports still resolve to known exports");
        assert!(imported.contains("b_fn"));
    }
}
