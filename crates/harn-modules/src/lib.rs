use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Component, Path, PathBuf};

use harn_lexer::Span;
use harn_parser::{BindingPattern, Node, Parser, SNode};
use serde::Deserialize;

pub mod asset_paths;
mod stdlib;

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
    /// Every `fn` declaration at module scope, used to implement the
    /// fallback "no `pub fn` → export everything" rule that matches the
    /// runtime loader's behavior.
    fn_names: Vec<String>,
    /// True when at least one `pub fn` appeared at module scope.
    has_pub_fn: bool,
    /// Top-level type-like declarations that can be imported into a caller's
    /// static type environment.
    type_declarations: Vec<SNode>,
}

#[derive(Debug, Clone)]
struct ImportRef {
    path: Option<PathBuf>,
    selective_names: Option<HashSet<String>>,
}

#[derive(Debug, Default, Deserialize)]
struct PackageManifest {
    #[serde(default)]
    exports: HashMap<String, String>,
}

/// Build a module graph from a set of files.
///
/// Files referenced via `import` statements are loaded recursively so the
/// graph contains every module reachable from the seed set. Cycles and
/// already-loaded files are skipped via a visited set.
pub fn build(files: &[PathBuf]) -> ModuleGraph {
    let mut modules: HashMap<PathBuf, ModuleInfo> = HashMap::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut queue: VecDeque<PathBuf> = VecDeque::new();
    for file in files {
        let canonical = normalize_path(file);
        if seen.insert(canonical.clone()) {
            queue.push_back(canonical);
        }
    }
    while let Some(path) = queue.pop_front() {
        if modules.contains_key(&path) {
            continue;
        }
        let module = load_module(&path);
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
                    queue.push_back(canonical);
                }
            }
        }
        modules.insert(path, module);
    }
    resolve_re_exports(&mut modules);
    ModuleGraph { modules }
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

/// Resolve an import string relative to the importing file.
///
/// Returns the path as-constructed (not canonicalized) so callers that
/// compare against their own `PathBuf::join` result get matching values.
/// The module graph canonicalizes internally via `normalize_path` when
/// keying modules, so call-site canonicalization is not required for
/// dedup.
///
/// `std/<module>` imports resolve to a virtual path (`<std>/<module>`)
/// backed by the embedded stdlib sources in [`stdlib`]. This lets the
/// module graph model stdlib symbols even though they have no on-disk
/// location.
pub fn resolve_import_path(current_file: &Path, import_path: &str) -> Option<PathBuf> {
    if let Some(module) = import_path.strip_prefix("std/") {
        if stdlib::get_stdlib_source(module).is_some() {
            return Some(stdlib::stdlib_virtual_path(module));
        }
        return None;
    }

    let base = current_file.parent().unwrap_or(Path::new("."));
    let mut file_path = base.join(import_path);
    if !file_path.exists() && file_path.extension().is_none() {
        file_path.set_extension("harn");
    }
    if file_path.exists() {
        return Some(file_path);
    }

    if let Some(path) = resolve_package_import(base, import_path) {
        return Some(path);
    }

    None
}

fn resolve_package_import(base: &Path, import_path: &str) -> Option<PathBuf> {
    for anchor in base.ancestors() {
        let packages_root = anchor.join(".harn/packages");
        if !packages_root.is_dir() {
            if anchor.join(".git").exists() {
                break;
            }
            continue;
        }
        if let Some(path) = resolve_from_packages_root(&packages_root, import_path) {
            return Some(path);
        }
        if anchor.join(".git").exists() {
            break;
        }
    }
    None
}

fn resolve_from_packages_root(packages_root: &Path, import_path: &str) -> Option<PathBuf> {
    let safe_import_path = safe_package_relative_path(import_path)?;
    let package_name = package_name_from_relative_path(&safe_import_path)?;
    let package_root = packages_root.join(package_name);

    let pkg_path = packages_root.join(&safe_import_path);
    if let Some(path) = finalize_package_target(&package_root, &pkg_path) {
        return Some(path);
    }

    let export_name = export_name_from_relative_path(&safe_import_path)?;
    let manifest_path = packages_root.join(package_name).join("harn.toml");
    let manifest = read_package_manifest(&manifest_path)?;
    let rel_path = manifest.exports.get(export_name)?;
    let safe_export_path = safe_package_relative_path(rel_path)?;
    finalize_package_target(&package_root, &package_root.join(safe_export_path))
}

fn read_package_manifest(path: &Path) -> Option<PackageManifest> {
    let content = std::fs::read_to_string(path).ok()?;
    toml::from_str::<PackageManifest>(&content).ok()
}

fn safe_package_relative_path(raw: &str) -> Option<PathBuf> {
    if raw.is_empty() || raw.contains('\\') {
        return None;
    }
    let mut out = PathBuf::new();
    let mut saw_component = false;
    for component in Path::new(raw).components() {
        match component {
            Component::Normal(part) => {
                saw_component = true;
                out.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    saw_component.then_some(out)
}

fn package_name_from_relative_path(path: &Path) -> Option<&str> {
    match path.components().next()? {
        Component::Normal(name) => name.to_str(),
        _ => None,
    }
}

fn export_name_from_relative_path(path: &Path) -> Option<&str> {
    let mut components = path.components();
    components.next()?;
    let rest = components.as_path();
    if rest.as_os_str().is_empty() {
        None
    } else {
        rest.to_str()
    }
}

fn path_is_within(root: &Path, path: &Path) -> bool {
    let Ok(root) = root.canonicalize() else {
        return false;
    };
    let Ok(path) = path.canonicalize() else {
        return false;
    };
    path == root || path.starts_with(root)
}

fn target_within_package_root(package_root: &Path, path: PathBuf) -> Option<PathBuf> {
    path_is_within(package_root, &path).then_some(path)
}

fn finalize_package_target(package_root: &Path, path: &Path) -> Option<PathBuf> {
    if path.is_dir() {
        let lib = path.join("lib.harn");
        if lib.exists() {
            return target_within_package_root(package_root, lib);
        }
        return target_within_package_root(package_root, path.to_path_buf());
    }
    if path.exists() {
        return target_within_package_root(package_root, path.to_path_buf());
    }
    if path.extension().is_none() {
        let mut with_ext = path.to_path_buf();
        with_ext.set_extension("harn");
        if with_ext.exists() {
            return target_within_package_root(package_root, with_ext);
        }
    }
    None
}

impl ModuleGraph {
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
    /// - selectively imported names that exist either as local
    ///   declarations in their target module or as a re-exported name —
    ///   matching what the VM accepts at runtime.
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
            match &import.selective_names {
                None => {
                    names.extend(imported.exports.iter().cloned());
                }
                Some(selective) => {
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

    /// Find the definition of `name` visible from `file`.
    ///
    /// Recurses through `pub import` re-export chains so go-to-definition
    /// lands on the symbol's actual declaration site instead of the facade
    /// module that forwarded it.
    pub fn definition_of(&self, file: &Path, name: &str) -> Option<DefSite> {
        let mut visited = HashSet::new();
        self.definition_of_inner(file, name, &mut visited)
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
}

/// A duplicate or ambiguous re-export inside a single module. Reported by
/// [`ModuleGraph::re_export_conflicts`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReExportConflict {
    pub name: String,
    pub sources: Vec<PathBuf>,
}

fn load_module(path: &Path) -> ModuleInfo {
    // `<std>/<name>` virtual paths map to the embedded stdlib source
    // rather than a real file on disk.
    let source = if let Some(stdlib_module) = stdlib_module_from_path(path) {
        match stdlib::get_stdlib_source(stdlib_module) {
            Some(src) => src.to_string(),
            None => return ModuleInfo::default(),
        }
    } else {
        match std::fs::read_to_string(path) {
            Ok(src) => src,
            Err(_) => return ModuleInfo::default(),
        }
    };
    let mut lexer = harn_lexer::Lexer::new(&source);
    let tokens = match lexer.tokenize() {
        Ok(tokens) => tokens,
        Err(_) => return ModuleInfo::default(),
    };
    let mut parser = Parser::new(tokens);
    let program = match parser.parse() {
        Ok(program) => program,
        Err(_) => return ModuleInfo::default(),
    };

    let mut module = ModuleInfo::default();
    for node in &program {
        collect_module_info(path, node, &mut module);
        collect_type_declarations(node, &mut module.type_declarations);
    }
    // Fallback matching the VM loader: if the module declares no
    // `pub fn`, every fn is implicitly exported.
    if !module.has_pub_fn {
        for name in &module.fn_names {
            module.own_exports.insert(name.clone());
        }
    }
    // Seed the transitive `exports` set from local exports plus selective
    // re-export names. Wildcard re-exports are folded in by
    // [`resolve_re_exports`] after every module has been loaded.
    module.exports.extend(module.own_exports.iter().cloned());
    module
        .exports
        .extend(module.selective_re_exports.keys().cloned());
    module
}

/// Extract the stdlib module name when `path` is a `<std>/<name>`
/// virtual path, otherwise `None`.
fn stdlib_module_from_path(path: &Path) -> Option<&str> {
    let s = path.to_str()?;
    s.strip_prefix("<std>/")
}

fn collect_module_info(file: &Path, snode: &SNode, module: &mut ModuleInfo) {
    match &snode.node {
        Node::FnDecl {
            name,
            params,
            is_pub,
            ..
        } => {
            if *is_pub {
                module.own_exports.insert(name.clone());
                module.has_pub_fn = true;
            }
            module.fn_names.push(name.clone());
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
        Node::TypeDecl { name, .. } => {
            module.own_exports.insert(name.clone());
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Type),
            );
        }
        Node::LetBinding { pattern, .. } | Node::VarBinding { pattern, .. } => {
            for name in pattern_names(pattern) {
                module.declarations.insert(
                    name.clone(),
                    decl_site(file, snode.span, &name, DefKind::Variable),
                );
            }
        }
        Node::ImportDecl { path, is_pub } => {
            let import_path = resolve_import_path(file, path);
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
                path: import_path,
                selective_names: None,
            });
        }
        Node::SelectiveImport {
            names,
            path,
            is_pub,
        } => {
            let import_path = resolve_import_path(file, path);
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
                path: import_path,
                selective_names: Some(names),
            });
        }
        Node::AttributedDecl { inner, .. } => {
            collect_module_info(file, inner, module);
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

fn type_decl_name(snode: &SNode) -> Option<&str> {
    match &snode.node {
        Node::TypeDecl { name, .. }
        | Node::StructDecl { name, .. }
        | Node::EnumDecl { name, .. }
        | Node::InterfaceDecl { name, .. } => Some(name.as_str()),
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
    if stdlib_module_from_path(path).is_some() {
        return path.to_path_buf();
    }
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_file(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, contents).unwrap();
        path
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
    fn runtime_stdlib_import_surface_resolves_to_embedded_sources() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = write_file(tmp.path(), "entry.harn", "");

        for (module, _) in stdlib::STDLIB_SOURCES {
            let import_path = format!("std/{module}");
            assert!(
                resolve_import_path(&entry, &import_path).is_some(),
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
    fn package_export_map_resolves_declared_module() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let packages = root.join(".harn/packages/acme/runtime");
        fs::create_dir_all(&packages).unwrap();
        fs::write(
            root.join(".harn/packages/acme/harn.toml"),
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

    #[test]
    fn package_direct_import_cannot_escape_packages_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join(".harn/packages/acme")).unwrap();
        fs::write(root.join("secret.harn"), "pub fn leaked() { 1 }\n").unwrap();
        let entry = write_file(root, "entry.harn", "");

        let resolved = resolve_import_path(&entry, "acme/../../secret");
        assert!(resolved.is_none(), "package import escaped package root");
    }

    #[test]
    fn package_export_map_cannot_escape_package_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join(".harn/packages/acme")).unwrap();
        fs::write(root.join("secret.harn"), "pub fn leaked() { 1 }\n").unwrap();
        fs::write(
            root.join(".harn/packages/acme/harn.toml"),
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
        fs::create_dir_all(root.join(".harn/packages")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&source, root.join(".harn/packages/acme")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&source, root.join(".harn/packages/acme")).unwrap();
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
        fs::create_dir_all(root.join(".harn/packages/acme")).unwrap();
        fs::create_dir_all(root.join(".harn/packages/shared")).unwrap();
        fs::write(
            root.join(".harn/packages/shared/lib.harn"),
            "pub fn shared_helper() { 1 }\n",
        )
        .unwrap();
        fs::write(
            root.join(".harn/packages/acme/lib.harn"),
            "import \"shared\"\npub fn use_shared() { shared_helper() }\n",
        )
        .unwrap();
        let entry = write_file(root, "entry.harn", "import \"acme\"\nuse_shared()\n");

        let graph = build(std::slice::from_ref(&entry));
        let imported = graph
            .imported_names_for_file(&entry)
            .expect("nested package import should resolve");
        assert!(imported.contains("use_shared"));
        let acme_path = root.join(".harn/packages/acme/lib.harn");
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
            "expected exactly one re-export conflict, got {:?}",
            conflicts
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
