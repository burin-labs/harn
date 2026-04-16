use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use harn_lexer::Span;
use harn_parser::{BindingPattern, Node, Parser, SNode};

mod stdlib;

/// Kind of symbol that can be exported by a module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefKind {
    Function,
    Pipeline,
    Tool,
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
    /// Public exports exposed by wildcard imports.
    exports: HashSet<String>,
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
}

#[derive(Debug, Clone)]
struct ImportRef {
    path: Option<PathBuf>,
    selective_names: Option<HashSet<String>>,
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
        for import in &module.imports {
            if let Some(import_path) = &import.path {
                if seen.insert(import_path.clone()) {
                    queue.push_back(import_path.clone());
                }
            }
        }
        modules.insert(path, module);
    }
    ModuleGraph { modules }
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

    let pkg_path = base.join(".harn/packages").join(import_path);
    if pkg_path.exists() {
        if pkg_path.is_dir() {
            let lib = pkg_path.join("lib.harn");
            if lib.exists() {
                return Some(lib);
            }
        }
        return Some(pkg_path);
    }

    let mut pkg_harn = pkg_path;
    pkg_harn.set_extension("harn");
    if pkg_harn.exists() {
        return Some(pkg_harn);
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
    /// - all public exports from wildcard-imported modules, and
    /// - selectively imported names that exist as declarations in their
    ///   target module (unresolvable selective names are still included,
    ///   but only if the target file itself resolved — if the file failed
    ///   to parse, the caller will see `None`).
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
                        if imported.declarations.contains_key(name) {
                            names.insert(name.clone());
                        }
                    }
                }
            }
        }
        Some(names)
    }

    /// Find the definition of `name` visible from `file`.
    pub fn definition_of(&self, file: &Path, name: &str) -> Option<DefSite> {
        let file = normalize_path(file);
        let current = self.modules.get(&file)?;

        if let Some(local) = current.declarations.get(name) {
            return Some(local.clone());
        }

        for import in &current.imports {
            if let Some(selective_names) = &import.selective_names {
                if !selective_names.contains(name) {
                    continue;
                }
            } else {
                continue;
            }

            if let Some(path) = &import.path {
                if let Some(symbol) = self
                    .modules
                    .get(path)
                    .or_else(|| self.modules.get(&normalize_path(path)))
                    .and_then(|module| module.declarations.get(name))
                {
                    return Some(symbol.clone());
                }
            }
        }

        for import in &current.imports {
            if import.selective_names.is_some() {
                continue;
            }
            if let Some(path) = &import.path {
                if let Some(symbol) = self
                    .modules
                    .get(path)
                    .or_else(|| self.modules.get(&normalize_path(path)))
                    .and_then(|module| module.declarations.get(name))
                {
                    return Some(symbol.clone());
                }
            }
        }

        None
    }
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
    }
    // Fallback matching the VM loader: if the module declares no
    // `pub fn`, every fn is implicitly exported.
    if !module.has_pub_fn {
        for name in &module.fn_names {
            module.exports.insert(name.clone());
        }
    }
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
                module.exports.insert(name.clone());
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
                module.exports.insert(name.clone());
            }
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Pipeline),
            );
        }
        Node::ToolDecl { name, is_pub, .. } => {
            if *is_pub {
                module.exports.insert(name.clone());
            }
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Tool),
            );
        }
        Node::StructDecl { name, is_pub, .. } => {
            if *is_pub {
                module.exports.insert(name.clone());
            }
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Struct),
            );
        }
        Node::EnumDecl { name, is_pub, .. } => {
            if *is_pub {
                module.exports.insert(name.clone());
            }
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Enum),
            );
        }
        Node::InterfaceDecl { name, .. } => {
            module.exports.insert(name.clone());
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Interface),
            );
        }
        Node::TypeDecl { name, .. } => {
            module.exports.insert(name.clone());
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
        Node::ImportDecl { path } => {
            let import_path = resolve_import_path(file, path);
            if import_path.is_none() {
                module.has_unresolved_wildcard_import = true;
            }
            module.imports.push(ImportRef {
                path: import_path,
                selective_names: None,
            });
        }
        Node::SelectiveImport { names, path } => {
            let import_path = resolve_import_path(file, path);
            if import_path.is_none() {
                module.has_unresolved_selective_import = true;
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
}
