//! The override family of a method for `change_signature`: every
//! declaration that must take the new parameter list together with it.
//!
//! Call sites are matched by name, so a method can change only when every
//! declaration that shares its name changes with it. That set is sound to
//! rewrite when it is exactly one family: one root declaration (a trait,
//! interface or base-class method) and members whose owners all descend
//! from the root's owner through supertypes this workspace declares.
//!
//! Enumeration reads every in-scope file of the method's language. It
//! refuses when a namesake is not a family member (a free function, an
//! inherent method, an unrelated class), when a declaration sits where it
//! cannot be read (inside a macro, an object literal, a class field), when
//! any supertype on a member's chain cannot be resolved to one workspace
//! declaration (an external trait or package class, a mixin call, a name
//! two declarations share), or when a member file is generated.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};

use tree_sitter::Node;

use crate::ast::{api as ast_api, Language};
use crate::error::HostlibError;

use super::refactor_core::{files_in_scope, first_syntax_error, read_source, Scope, SeedCandidate};
use super::signature_syntax::{named_children, text, Family};
use super::state::IndexState;

/// Every declaration that changes together, the seed included.
pub(super) struct OverrideFamily {
    /// `(path, 1-based line of the name)` per member.
    pub members: Vec<(String, u32)>,
    /// Names of the classes and types that declare a member.
    pub owners: Vec<String>,
}

/// A location that keeps the family from being enumerated soundly.
pub(super) struct Blocker {
    pub path: String,
    pub line: usize,
    pub kind: &'static str,
    pub reason: String,
}

pub(super) struct FamilyRefusal {
    pub tag: &'static str,
    pub details: String,
    pub sites: Vec<Blocker>,
}

/// Names the Rust prelude brings into every module: a workspace trait with
/// one of them cannot be told apart from the std trait by name.
const RUST_PRELUDE_TRAITS: &[&str] = &[
    "AsMut",
    "AsRef",
    "Clone",
    "Copy",
    "Default",
    "DoubleEndedIterator",
    "Drop",
    "Eq",
    "ExactSizeIterator",
    "Extend",
    "Fn",
    "FnMut",
    "FnOnce",
    "From",
    "FromIterator",
    "Into",
    "IntoIterator",
    "Iterator",
    "Ord",
    "PartialEq",
    "PartialOrd",
    "Send",
    "Sized",
    "Sync",
    "ToOwned",
    "ToString",
    "TryFrom",
    "TryInto",
    "Unpin",
];

/// Python bases that declare no user methods, by module.
const PYTHON_NEUTRAL_BASES: &[(&str, &str)] = &[
    ("abc", "ABC"),
    ("typing", "Protocol"),
    ("typing", "Generic"),
    ("typing_extensions", "Protocol"),
    ("typing_extensions", "Generic"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OwnerKind {
    RustTrait,
    RustTraitImpl,
    Class,
    Interface,
}

/// A supertype reference as written on an owner.
#[derive(Clone, Debug)]
enum BaseRef {
    /// A name to resolve: same file, an import, or (Rust) the one
    /// workspace trait that declares the method.
    Name(String),
    /// A base known to declare no methods (`object`, `abc.ABC`).
    Neutral,
    /// A base that cannot be resolved, and why.
    Opaque(String),
}

#[derive(Clone, Debug)]
struct OwnerDecl {
    name: Option<String>,
    line: usize,
    kind: OwnerKind,
    bases: Vec<BaseRef>,
}

#[derive(Clone, Debug)]
struct RawMember {
    /// 1-based line of the declaration's name.
    line: usize,
    /// 1-based first line, decorators and attributes included.
    first_line: usize,
    owner: Result<usize, String>,
}

/// An import binding: `local` names `original` from `module`.
#[derive(Clone, Debug)]
struct Import {
    local: String,
    module: String,
    original: String,
}

/// A Rust `use` leaf: the name it binds and the first segment of its path.
#[derive(Clone, Debug)]
struct RustUse {
    bound: Option<String>,
    root: String,
}

#[derive(Default)]
struct FileScan {
    members: Vec<RawMember>,
    blockers: Vec<(usize, &'static str, String)>,
    owners: Vec<OwnerDecl>,
    imports: Vec<Import>,
    rust_uses: Vec<RustUse>,
    /// Python names bound to a neutral base (`from abc import ABC`).
    neutral_names: BTreeSet<String>,
}

/// One owner, addressed by file and index into that file's owners.
type OwnerRef = (String, usize);

struct Enumerator<'a> {
    builtin: &'static str,
    state: &'a IndexState,
    session: Option<&'a str>,
    family: Family,
    name: &'a str,
    scans: HashMap<String, Option<FileScan>>,
    workspace_paths: BTreeSet<String>,
}

/// Enumerate the override family of the method `name` declared at
/// `seed_path:seed_line`. `in_scope` is every file that may mention the
/// name; `competing` the symbol graph's other declarations of it; `bound`
/// whether a trait or interface binds the seed.
///
/// A lone method (no namesake, not bound) keeps the single-declaration
/// contract: its own supertypes are not resolved. Types inheriting it are
/// still enumerated.
#[allow(clippy::too_many_arguments)]
pub(super) fn enumerate(
    builtin: &'static str,
    state: &IndexState,
    family: Family,
    name: &str,
    (seed_path, seed_line): (&str, u32),
    bound: bool,
    in_scope: &[String],
    competing: &[SeedCandidate],
    session: Option<&str>,
) -> Result<Result<OverrideFamily, FamilyRefusal>, HostlibError> {
    let mut enumerator = Enumerator {
        builtin,
        state,
        session,
        family,
        name,
        scans: HashMap::new(),
        workspace_paths: state.path_to_id.keys().cloned().collect(),
    };
    let mut blockers: Vec<Blocker> = Vec::new();

    if family == Family::Python && name.starts_with("__") && name.ends_with("__") {
        return Ok(Err(FamilyRefusal {
            tag: "overrides_present",
            details: format!(
                "`{name}` is a special method that the language calls implicitly, so its call sites cannot be enumerated; change it by hand"
            ),
            sites: Vec::new(),
        }));
    }

    let mut files: Vec<String> = in_scope
        .iter()
        .filter(|path| {
            Language::detect(Path::new(path.as_str()), None).and_then(Family::of) == Some(family)
        })
        .cloned()
        .collect();
    if !files.iter().any(|path| path == seed_path) {
        files.push(seed_path.to_string());
    }
    files.sort();
    files.dedup();

    // path -> members found in it.
    let mut members: BTreeMap<String, Vec<RawMember>> = BTreeMap::new();
    for path in &files {
        let Some(scan) = enumerator.scan(path)? else {
            return Ok(Err(FamilyRefusal {
                tag: "syntax_error",
                details: format!(
                    "`{path}` does not parse, so the declarations of `{name}` in it cannot be enumerated; fix it first"
                ),
                sites: Vec::new(),
            }));
        };
        for (line, kind, reason) in &scan.blockers {
            block(&mut blockers, path, *line, kind, reason.clone());
        }
        if !scan.members.is_empty() {
            members.insert(path.clone(), scan.members.clone());
        }
    }

    let seed_found = members.get(seed_path).is_some_and(|found| {
        found
            .iter()
            .any(|m| (m.first_line..=m.line).contains(&(seed_line as usize)))
    });
    if !seed_found {
        block(
            &mut blockers,
            seed_path,
            seed_line as usize,
            "declaration",
            "the indexed declaration does not parse as a method here; rebuild the index".into(),
        );
    }
    for (path, line, kind) in competing {
        let matched = members.get(path).is_some_and(|found| {
            found
                .iter()
                .any(|m| (m.first_line..=m.line).contains(&(*line as usize)))
        });
        let reported = blockers
            .iter()
            .any(|b| b.path == *path && b.line == *line as usize);
        if !matched && !reported {
            block(
                &mut blockers,
                path,
                *line as usize,
                kind,
                format!("a {kind} named `{name}` that is not a method declaration"),
            );
        }
    }

    for path in members.keys() {
        if let Some(reason) = generated_marker(builtin, state, path, session)? {
            block(&mut blockers, path, 1, "generated", reason);
        }
    }

    let lone = !bound && competing.is_empty() && members.values().map(Vec::len).sum::<usize>() == 1;

    // Owner of each member; members without one are namesakes outside any family.
    let mut member_owners: Vec<(String, usize, OwnerRef)> = Vec::new();
    for (path, found) in &members {
        for member in found {
            match &member.owner {
                Ok(index) => {
                    member_owners.push((path.clone(), member.line, (path.clone(), *index)));
                }
                // A lone free-standing or inherent method has no family.
                Err(_) if lone && blockers.is_empty() => {
                    return Ok(Ok(OverrideFamily {
                        members: vec![(path.clone(), member.line as u32)],
                        owners: Vec::new(),
                    }));
                }
                Err(reason) => block(
                    &mut blockers,
                    path,
                    member.line,
                    "declaration",
                    reason.clone(),
                ),
            }
        }
    }

    // Two declarations in one owner are overloads.
    let mut per_owner: BTreeMap<&OwnerRef, Vec<(&str, usize)>> = BTreeMap::new();
    for (path, line, owner) in &member_owners {
        per_owner.entry(owner).or_default().push((path, *line));
    }
    for sites in per_owner.values().filter(|sites| sites.len() > 1) {
        for (path, line) in sites {
            block(
                &mut blockers,
                path,
                *line,
                "overload",
                format!("one of several declarations of `{name}` in the same type"),
            );
        }
    }

    // Supertypes of every member owner, transitively.
    let mut ancestors: HashMap<OwnerRef, BTreeSet<OwnerRef>> = HashMap::new();
    let mut stack: Vec<OwnerRef> = member_owners.iter().map(|(_, _, o)| o.clone()).collect();
    let mut direct: HashMap<OwnerRef, Vec<OwnerRef>> = HashMap::new();
    while let Some(owner) = stack.pop() {
        if direct.contains_key(&owner) {
            continue;
        }
        let decl = enumerator.owner(&owner).clone();
        let mut parents = Vec::new();
        for base in &decl.bases {
            match base {
                BaseRef::Neutral => {}
                // A lone method's supertypes stay unchecked, as before families.
                BaseRef::Opaque(_) | BaseRef::Name(_) if lone => {}
                BaseRef::Opaque(reason) => {
                    block(
                        &mut blockers,
                        &owner.0,
                        decl.line,
                        "supertype",
                        reason.clone(),
                    );
                }
                BaseRef::Name(base_name) => {
                    match enumerator.resolve_base(&owner, base_name, &member_owners)? {
                        Ok(parent) => {
                            stack.push(parent.clone());
                            parents.push(parent);
                        }
                        Err(reason) => {
                            block(&mut blockers, &owner.0, decl.line, "supertype", reason);
                        }
                    }
                }
            }
        }
        direct.insert(owner, parents);
    }
    if !blockers.is_empty() {
        return Ok(Err(refusal(name, blockers)));
    }
    for (_, _, owner) in &member_owners {
        let mut seen = BTreeSet::new();
        let mut walk = vec![owner.clone()];
        while let Some(current) = walk.pop() {
            if seen.insert(current.clone()) {
                walk.extend(direct.get(&current).cloned().unwrap_or_default());
            }
        }
        ancestors.insert(owner.clone(), seen);
    }

    // One member's owner must be an ancestor (or self) of every member's owner.
    let root = member_owners.iter().find(|(_, _, candidate)| {
        member_owners
            .iter()
            .all(|(_, _, owner)| ancestors[owner].contains(candidate))
    });
    if root.is_none() {
        for (path, line, _) in &member_owners {
            block(
                &mut blockers,
                path,
                *line,
                "declaration",
                format!("declarations of `{name}` that no single trait, interface or base class relates"),
            );
        }
        return Ok(Err(refusal(name, blockers)));
    }

    // Types that inherit the method without declaring it belong to the
    // family too: their other supertypes must resolve, and their names
    // qualify explicit-receiver calls.
    let root = root.expect("checked above").2.clone();
    let mut related: BTreeSet<OwnerRef> = BTreeSet::new();
    for owner in direct.keys() {
        let mut seen = BTreeSet::new();
        let mut walk = vec![owner.clone()];
        while let Some(current) = walk.pop() {
            if seen.insert(current.clone()) {
                walk.extend(direct.get(&current).cloned().unwrap_or_default());
            }
        }
        if seen.contains(&root) {
            related.insert(owner.clone());
        }
    }
    if family != Family::Rust {
        enumerator.descendants(&mut related, seed_path, &member_owners, &mut blockers)?;
        if !blockers.is_empty() {
            return Ok(Err(refusal(name, blockers)));
        }
    }

    let mut owners: Vec<String> = related
        .iter()
        .filter_map(|owner| enumerator.owner(owner).name.clone())
        .collect();
    owners.sort();
    owners.dedup();
    Ok(Ok(OverrideFamily {
        members: member_owners
            .iter()
            .map(|(path, line, _)| (path.clone(), *line as u32))
            .collect(),
        owners,
    }))
}

fn block(blockers: &mut Vec<Blocker>, path: &str, line: usize, kind: &'static str, reason: String) {
    blockers.push(Blocker {
        path: path.to_string(),
        line,
        kind,
        reason,
    });
}

fn refusal(name: &str, mut sites: Vec<Blocker>) -> FamilyRefusal {
    sites.sort_by(|a, b| (&a.path, a.line, a.kind).cmp(&(&b.path, b.line, b.kind)));
    sites.dedup_by(|a, b| (&a.path, a.line, a.kind) == (&b.path, b.line, b.kind));
    FamilyRefusal {
        tag: "overrides_present",
        details: format!(
            "`{name}` belongs to an override family that cannot be enumerated soundly (see `sites`); change those declarations by hand, or make every declaration of `{name}` part of one workspace trait, interface or class hierarchy"
        ),
        sites,
    }
}

/// Why a member file reads as generated, from its first lines.
fn generated_marker(
    builtin: &'static str,
    state: &IndexState,
    path: &str,
    session: Option<&str>,
) -> Result<Option<String>, HostlibError> {
    let source = read_source(builtin, &state.root, path, session)?;
    let head: String = source.lines().take(5).collect::<Vec<_>>().join("\n");
    let lower = head.to_ascii_lowercase();
    for marker in [
        "@generated",
        "do not edit",
        "auto-generated",
        "autogenerated",
    ] {
        if lower.contains(marker) {
            return Ok(Some(format!(
                "the file is marked generated (`{marker}`); edit its generator instead"
            )));
        }
    }
    Ok(None)
}

impl Enumerator<'_> {
    /// Scan `path`, caching the result. `None` when it does not parse.
    fn scan(&mut self, path: &str) -> Result<Option<&FileScan>, HostlibError> {
        if !self.scans.contains_key(path) {
            let scan = self.scan_uncached(path)?;
            self.scans.insert(path.to_string(), scan);
        }
        Ok(self.scans[path].as_ref())
    }

    fn scan_uncached(&self, path: &str) -> Result<Option<FileScan>, HostlibError> {
        let Some(language) = Language::detect(Path::new(path), None) else {
            return Ok(None);
        };
        let source = read_source(self.builtin, &self.state.root, path, self.session)?;
        if first_syntax_error(&source, language).is_some() {
            return Ok(None);
        }
        let Ok(tree) = ast_api::parse_tree(&source, language) else {
            return Ok(None);
        };
        let crate_roots = if self.family == Family::Rust {
            rust_crate_names(&self.state.root, path)
        } else {
            Vec::new()
        };
        Ok(Some(scan_file(
            tree.root_node(),
            source.as_bytes(),
            self.family,
            self.name,
            &crate_roots,
        )))
    }

    fn owner(&self, owner: &OwnerRef) -> &OwnerDecl {
        &self.scans[&owner.0]
            .as_ref()
            .expect("owners come from parsed files")
            .owners[owner.1]
    }

    /// Resolve one supertype name written on `owner` to a workspace owner.
    fn resolve_base(
        &mut self,
        owner: &OwnerRef,
        base: &str,
        member_owners: &[(String, usize, OwnerRef)],
    ) -> Result<Result<OwnerRef, String>, HostlibError> {
        if self.family == Family::Rust {
            // The trait must declare the method, so it is a member's owner.
            let mut traits: Vec<OwnerRef> = member_owners
                .iter()
                .map(|(_, _, o)| o.clone())
                .filter(|o| {
                    let d = self.owner(o);
                    d.kind == OwnerKind::RustTrait && d.name.as_deref() == Some(base)
                })
                .collect();
            traits.sort();
            traits.dedup();
            return Ok(match traits.as_slice() {
                [only] => Ok(only.clone()),
                [] => Err(format!(
                    "implements trait `{base}`, which no workspace trait declaring `{}` matches; the trait is outside the workspace",
                    self.name
                )),
                _ => Err(format!("several workspace traits named `{base}` declare `{}`", self.name)),
            });
        }
        let (path, _) = owner;
        let scan = self.scan(path)?.expect("owner files parsed");
        let local: Vec<usize> = owner_indices(scan, base);
        match local.as_slice() {
            [only] => return Ok(Ok((path.clone(), *only))),
            [] => {}
            _ => {
                return Ok(Err(format!(
                    "supertype `{base}` names several declarations in `{path}`"
                )))
            }
        }
        let imports: Vec<Import> = scan
            .imports
            .iter()
            .filter(|i| i.local == base)
            .cloned()
            .collect();
        let [import] = imports.as_slice() else {
            return Ok(Err(if imports.is_empty() {
                format!("supertype `{base}` is neither declared nor imported by name in `{path}`, so it may come from outside the workspace")
            } else {
                format!("supertype `{base}` is imported more than once in `{path}`")
            }));
        };
        let Some(target) = self.resolve_module(path, &import.module) else {
            return Ok(Err(format!(
                "supertype `{base}` comes from `{}`, which is not a workspace file",
                import.module
            )));
        };
        let Some(scan) = self.scan(&target)? else {
            return Ok(Err(format!("`{target}` does not parse")));
        };
        Ok(match owner_indices(scan, &import.original).as_slice() {
            [only] => Ok((target, *only)),
            [] => Err(format!(
                "supertype `{base}` is not declared in `{target}` (a re-export cannot be followed)"
            )),
            _ => Err(format!(
                "supertype `{base}` names several declarations in `{target}`"
            )),
        })
    }

    /// Grow `related` with every workspace type that names a related type
    /// as a supertype, transitively, blocking any whose other supertypes
    /// cannot be resolved: an unresolved one may supply the method.
    fn descendants(
        &mut self,
        related: &mut BTreeSet<OwnerRef>,
        seed_path: &str,
        member_owners: &[(String, usize, OwnerRef)],
        blockers: &mut Vec<Blocker>,
    ) -> Result<(), HostlibError> {
        let mut searched: BTreeSet<String> = BTreeSet::new();
        let mut checked: BTreeSet<OwnerRef> = related.clone();
        loop {
            let pending: Vec<String> = related
                .iter()
                .filter_map(|owner| self.owner(owner).name.clone())
                .filter(|owner_name| !searched.contains(owner_name))
                .collect();
            if pending.is_empty() {
                return Ok(());
            }
            for owner_name in pending {
                searched.insert(owner_name.clone());
                let files = files_in_scope(
                    self.state,
                    Scope::Workspace,
                    &owner_name,
                    seed_path,
                    self.session,
                );
                for path in files {
                    if Language::detect(Path::new(&path), None).and_then(Family::of)
                        != Some(self.family)
                    {
                        continue;
                    }
                    let Some(scan) = self.scan(&path)? else {
                        continue;
                    };
                    let count = scan.owners.len();
                    for index in 0..count {
                        let candidate = (path.clone(), index);
                        if checked.contains(&candidate) {
                            continue;
                        }
                        let decl = self.owner(&candidate).clone();
                        let names_related = decl
                            .bases
                            .iter()
                            .any(|b| matches!(b, BaseRef::Name(n) if *n == owner_name));
                        if !names_related {
                            continue;
                        }
                        checked.insert(candidate.clone());
                        let mut descends = false;
                        let mut problems = Vec::new();
                        for base in &decl.bases {
                            match base {
                                BaseRef::Neutral => {}
                                BaseRef::Opaque(reason) => problems.push(reason.clone()),
                                BaseRef::Name(base_name) => {
                                    match self.resolve_base(&candidate, base_name, member_owners)? {
                                        Ok(parent) if related.contains(&parent) => descends = true,
                                        Ok(_) => {}
                                        // It may name the related type; it cannot be ruled out.
                                        Err(reason) if *base_name == owner_name => {
                                            descends = true;
                                            problems.push(reason);
                                        }
                                        Err(reason) => problems.push(reason),
                                    }
                                }
                            }
                        }
                        if !descends {
                            continue;
                        }
                        for reason in problems {
                            block(
                                blockers,
                                &path,
                                decl.line,
                                "supertype",
                                format!("inherits `{}` and {reason}", self.name),
                            );
                        }
                        related.insert(candidate);
                    }
                }
            }
        }
    }

    /// The workspace file an import's module specifier names.
    fn resolve_module(&self, from: &str, module: &str) -> Option<String> {
        let exists = |candidate: &str| self.workspace_paths.contains(candidate);
        match self.family {
            Family::TypeScript => {
                if !module.starts_with('.') {
                    return None;
                }
                let base = normalize(Path::new(from).parent()?.join(module))?;
                let stem = ["js", "jsx", "mjs", "cjs"]
                    .iter()
                    .find_map(|ext| base.strip_suffix(&format!(".{ext}")))
                    .unwrap_or(&base);
                [
                    base.clone(),
                    format!("{stem}.ts"),
                    format!("{stem}.tsx"),
                    format!("{stem}.d.ts"),
                    format!("{stem}/index.ts"),
                    format!("{stem}/index.tsx"),
                ]
                .into_iter()
                .find(|candidate| exists(candidate) && !candidate.ends_with(".js"))
            }
            Family::Python => {
                let dots = module.chars().take_while(|c| *c == '.').count();
                let rest = module.get(dots..).unwrap_or_default().replace('.', "/");
                if dots > 0 {
                    let mut dir = Path::new(from).parent()?.to_path_buf();
                    for _ in 1..dots {
                        dir = dir.parent()?.to_path_buf();
                    }
                    let base = if rest.is_empty() {
                        dir
                    } else {
                        dir.join(&rest)
                    };
                    let base = normalize(base)?;
                    [format!("{base}.py"), format!("{base}/__init__.py")]
                        .into_iter()
                        .find(|candidate| exists(candidate))
                } else {
                    let suffixes = [format!("{rest}.py"), format!("{rest}/__init__.py")];
                    let matches: Vec<&String> = self
                        .workspace_paths
                        .iter()
                        .filter(|path| {
                            suffixes
                                .iter()
                                .any(|s| *path == s || path.ends_with(&format!("/{s}")))
                        })
                        .collect();
                    match matches.as_slice() {
                        [only] => Some((*only).clone()),
                        _ => None,
                    }
                }
            }
            Family::Rust => None,
        }
    }
}

fn owner_indices(scan: &FileScan, name: &str) -> Vec<usize> {
    scan.owners
        .iter()
        .enumerate()
        .filter(|(_, o)| o.name.as_deref() == Some(name) && o.kind != OwnerKind::RustTraitImpl)
        .map(|(i, _)| i)
        .collect()
}

/// `path` with `.` and `..` folded, as a `/`-separated relative path.
fn normalize(path: PathBuf) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

/// The crate names `path`'s nearest manifest declares, as paths spell them.
fn rust_crate_names(root: &Path, path: &str) -> Vec<String> {
    let mut dir = root.join(path);
    while dir.pop() && dir.starts_with(root) {
        let Ok(raw) = std::fs::read_to_string(dir.join("Cargo.toml")) else {
            continue;
        };
        let Ok(manifest) = raw.parse::<toml::Table>() else {
            return Vec::new();
        };
        let mut names = Vec::new();
        for section in ["package", "lib"] {
            if let Some(name) = manifest
                .get(section)
                .and_then(|s| s.get("name"))
                .and_then(|n| n.as_str())
            {
                names.push(name.replace('-', "_"));
            }
        }
        if !names.is_empty() {
            return names;
        }
    }
    Vec::new()
}

// === Per-file scan ===

fn scan_file(
    root: Node<'_>,
    bytes: &[u8],
    family: Family,
    name: &str,
    crate_roots: &[String],
) -> FileScan {
    let mut scan = FileScan::default();
    let mut owner_index: HashMap<usize, usize> = HashMap::new();
    let mut stack = vec![root];
    let mut nodes = Vec::new();
    while let Some(node) = stack.pop() {
        nodes.push(node);
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    nodes.sort_by_key(|n| n.start_byte());
    for node in &nodes {
        if let Some(owner) = owner_decl(*node, bytes, family, crate_roots) {
            owner_index.insert(node.id(), scan.owners.len());
            scan.owners.push(owner);
        }
    }
    for node in &nodes {
        let node = *node;
        match family {
            Family::Rust => {
                if node.kind() == "use_declaration" {
                    if let Some(argument) = node.child_by_field_name("argument") {
                        rust_use_leaves(argument, bytes, None, &mut scan.rust_uses);
                    }
                }
            }
            Family::TypeScript => {
                if node.kind() == "import_statement" {
                    ts_imports(node, bytes, &mut scan.imports);
                }
            }
            Family::Python => {
                if node.kind() == "import_from_statement" {
                    python_imports(node, bytes, &mut scan.imports, &mut scan.neutral_names);
                }
            }
        }
        if let Some((line, kind, reason)) = opaque_declaration(node, bytes, family, name) {
            scan.blockers.push((line, kind, reason));
        }
        if family.declaration_kinds().contains(&node.kind()) {
            let Some(name_node) = node.child_by_field_name("name") else {
                continue;
            };
            if text(name_node, bytes) != name {
                continue;
            }
            if node.kind() == "variable_declarator"
                && !node.child_by_field_name("value").is_some_and(|v| {
                    matches!(
                        v.kind(),
                        "arrow_function" | "function_expression" | "function"
                    )
                })
            {
                continue;
            }
            let first = node
                .parent()
                .filter(|p| p.kind() == "decorated_definition")
                .unwrap_or(node);
            let first_line = leading_attribute_row(first).unwrap_or(first.start_position().row);
            scan.members.push(RawMember {
                line: name_node.start_position().row + 1,
                first_line: first_line + 1,
                owner: member_owner(node, bytes, family, name, &owner_index),
            });
        }
    }
    if family == Family::Python {
        let local: BTreeSet<String> = scan.owners.iter().filter_map(|o| o.name.clone()).collect();
        let mut neutral = scan.neutral_names.clone();
        // The builtin `object`, unless this file declares or imports its own.
        if !scan.imports.iter().any(|i| i.local == "object") {
            neutral.insert("object".to_string());
        }
        for base in scan.owners.iter_mut().flat_map(|o| o.bases.iter_mut()) {
            if matches!(base, BaseRef::Name(n) if neutral.contains(n) && !local.contains(n)) {
                *base = BaseRef::Neutral;
            }
        }
    }
    // Rust impls resolve their trait name against this file's `use` items.
    if family == Family::Rust {
        let declared: BTreeSet<String> = scan
            .owners
            .iter()
            .filter(|o| o.kind == OwnerKind::RustTrait)
            .filter_map(|o| o.name.clone())
            .collect();
        let allowed = |root: &str| {
            matches!(root, "crate" | "self" | "super") || crate_roots.iter().any(|r| r == root)
        };
        let uses = scan.rust_uses.clone();
        for owner in scan
            .owners
            .iter_mut()
            .filter(|o| o.kind == OwnerKind::RustTraitImpl)
        {
            for base in owner.bases.iter_mut() {
                let BaseRef::Name(trait_name) = base else {
                    continue;
                };
                let binding = uses
                    .iter()
                    .find(|u| u.bound.as_deref() == Some(trait_name.as_str()) && !allowed(&u.root));
                let external_glob = uses.iter().find(|u| u.bound.is_none() && !allowed(&u.root));
                if let Some(binding) = binding {
                    *base = BaseRef::Opaque(format!(
                        "implements `{trait_name}`, which this file imports from `{}`, outside the workspace",
                        binding.root
                    ));
                } else if !declared.contains(trait_name.as_str())
                    && !uses
                        .iter()
                        .any(|u| u.bound.as_deref() == Some(trait_name.as_str()))
                {
                    if let Some(glob) = external_glob {
                        *base = BaseRef::Opaque(format!(
                            "implements `{trait_name}`, which a glob import from `{}` may supply",
                            glob.root
                        ));
                    }
                }
            }
        }
    }
    scan
}

/// The row of the first attribute item directly above a Rust item.
fn leading_attribute_row(node: Node<'_>) -> Option<usize> {
    let mut row = None;
    let mut sibling = node.prev_sibling();
    while let Some(current) = sibling {
        match current.kind() {
            "attribute_item" => row = Some(current.start_position().row),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        sibling = current.prev_sibling();
    }
    row
}

/// An owner (trait, trait impl, class, interface) declared by `node`.
fn owner_decl(
    node: Node<'_>,
    bytes: &[u8],
    family: Family,
    crate_roots: &[String],
) -> Option<OwnerDecl> {
    let line = node.start_position().row + 1;
    let name_text = |field: &str| {
        node.child_by_field_name(field)
            .map(|n| text(n, bytes).to_string())
    };
    match (family, node.kind()) {
        (Family::Rust, "trait_item") => Some(OwnerDecl {
            name: name_text("name"),
            line,
            kind: OwnerKind::RustTrait,
            bases: Vec::new(),
        }),
        (Family::Rust, "impl_item") => {
            let trait_node = node.child_by_field_name("trait")?;
            Some(OwnerDecl {
                name: name_text("type"),
                line,
                kind: OwnerKind::RustTraitImpl,
                bases: vec![rust_trait_ref(trait_node, bytes, crate_roots)],
            })
        }
        (Family::TypeScript, "class_declaration" | "abstract_class_declaration" | "class") => {
            let mut bases = Vec::new();
            for child in named_children(node) {
                if child.kind() != "class_heritage" {
                    continue;
                }
                for clause in named_children(child) {
                    match clause.kind() {
                        "extends_clause" => {
                            let mut cursor = clause.walk();
                            for value in clause.children_by_field_name("value", &mut cursor) {
                                bases.push(match value.kind() {
                                    "identifier" => BaseRef::Name(text(value, bytes).to_string()),
                                    _ => BaseRef::Opaque(format!(
                                        "extends `{}`, which is not a plain class name",
                                        text(value, bytes)
                                    )),
                                });
                            }
                        }
                        "implements_clause" => {
                            for ty in named_children(clause) {
                                bases.push(ts_type_ref(ty, bytes));
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some(OwnerDecl {
                name: name_text("name"),
                line,
                kind: OwnerKind::Class,
                bases,
            })
        }
        (Family::TypeScript, "interface_declaration") => {
            let mut bases = Vec::new();
            for child in named_children(node) {
                if child.kind() == "extends_type_clause" {
                    for ty in named_children(child) {
                        bases.push(ts_type_ref(ty, bytes));
                    }
                }
            }
            Some(OwnerDecl {
                name: name_text("name"),
                line,
                kind: OwnerKind::Interface,
                bases,
            })
        }
        (Family::Python, "class_definition") => {
            let bases = node
                .child_by_field_name("superclasses")
                .map(|list| {
                    named_children(list)
                        .into_iter()
                        .filter(|c| c.kind() != "comment")
                        .map(|c| python_base_ref(c, bytes))
                        .collect()
                })
                .unwrap_or_default();
            Some(OwnerDecl {
                name: name_text("name"),
                line,
                kind: OwnerKind::Class,
                bases,
            })
        }
        _ => None,
    }
}

fn rust_trait_ref(node: Node<'_>, bytes: &[u8], crate_roots: &[String]) -> BaseRef {
    match node.kind() {
        "type_identifier" => {
            let name = text(node, bytes).to_string();
            if RUST_PRELUDE_TRAITS.contains(&name.as_str()) {
                BaseRef::Opaque(format!(
                    "implements `{name}`, a name the std prelude provides"
                ))
            } else {
                BaseRef::Name(name)
            }
        }
        "generic_type" => match node.child_by_field_name("type") {
            Some(inner) => rust_trait_ref(inner, bytes, crate_roots),
            None => BaseRef::Opaque(format!("implements `{}`", text(node, bytes))),
        },
        "scoped_type_identifier" => {
            let root = node
                .child_by_field_name("path")
                .map(|p| leftmost_text(p, bytes))
                .unwrap_or_default();
            let allowed =
                matches!(root.as_str(), "crate" | "self" | "super") || crate_roots.contains(&root);
            match node.child_by_field_name("name") {
                Some(name) if allowed => BaseRef::Name(text(name, bytes).to_string()),
                _ => BaseRef::Opaque(format!(
                    "implements `{}`, a trait outside the workspace",
                    text(node, bytes)
                )),
            }
        }
        _ => BaseRef::Opaque(format!("implements `{}`", text(node, bytes))),
    }
}

fn ts_type_ref(node: Node<'_>, bytes: &[u8]) -> BaseRef {
    match node.kind() {
        "type_identifier" | "identifier" => BaseRef::Name(text(node, bytes).to_string()),
        "generic_type" => match node.child_by_field_name("name") {
            Some(name) if name.kind() == "type_identifier" => {
                BaseRef::Name(text(name, bytes).to_string())
            }
            _ => BaseRef::Opaque(format!("extends `{}`", text(node, bytes))),
        },
        _ => BaseRef::Opaque(format!(
            "extends `{}`, which is not a plain type name",
            text(node, bytes)
        )),
    }
}

fn python_base_ref(node: Node<'_>, bytes: &[u8]) -> BaseRef {
    let written = text(node, bytes);
    let neutral_qualified = |t: &str| {
        PYTHON_NEUTRAL_BASES
            .iter()
            .any(|(module, base)| t == format!("{module}.{base}"))
    };
    match node.kind() {
        "identifier" => BaseRef::Name(written.to_string()),
        "attribute" if neutral_qualified(written) => BaseRef::Neutral,
        "subscript" => match node.child_by_field_name("value") {
            Some(value) => python_base_ref(value, bytes),
            None => BaseRef::Opaque(format!("extends `{written}`")),
        },
        "keyword_argument" => {
            let key = node.child_by_field_name("name").map(|n| text(n, bytes));
            let value = node.child_by_field_name("value").map(|n| text(n, bytes));
            match (key, value) {
                (Some("metaclass"), Some("ABCMeta" | "abc.ABCMeta")) => BaseRef::Neutral,
                _ => BaseRef::Opaque(format!("class keyword `{written}`")),
            }
        }
        _ => BaseRef::Opaque(format!(
            "extends `{written}`, which is not a plain class name"
        )),
    }
}

fn leftmost_text(mut node: Node<'_>, bytes: &[u8]) -> String {
    while let Some(child) = node.named_child(0) {
        node = child;
    }
    text(node, bytes).to_string()
}

/// The owner of a declaration named like the method, or why it has none.
fn member_owner(
    node: Node<'_>,
    bytes: &[u8],
    family: Family,
    name: &str,
    owners: &HashMap<usize, usize>,
) -> Result<usize, String> {
    let owner_of = |owner: Node<'_>| {
        owners
            .get(&owner.id())
            .copied()
            .ok_or_else(|| format!("`{name}` is declared in an unnamed type"))
    };
    match family {
        Family::Rust => {
            let mut ancestor = node.parent();
            while let Some(current) = ancestor {
                match current.kind() {
                    "trait_item" => return owner_of(current),
                    "impl_item" if current.child_by_field_name("trait").is_some() => {
                        return owner_of(current)
                    }
                    "impl_item" => {
                        let ty = current
                            .child_by_field_name("type")
                            .map(|t| text(t, bytes))
                            .unwrap_or("?");
                        return Err(format!(
                            "inherent method `{ty}::{name}` shares the name; calls by name cannot tell them apart"
                        ));
                    }
                    "function_item" | "mod_item" | "source_file" => break,
                    _ => ancestor = current.parent(),
                }
            }
            Err(format!(
                "free function `{name}` shares the name; calls by name cannot tell them apart"
            ))
        }
        Family::TypeScript => {
            let parent = node.parent();
            match (node.kind(), parent.map(|p| p.kind())) {
                ("method_definition", Some("class_body")) => {
                    if has_token(node, &["get", "set"]) {
                        return Err(format!("accessor `{name}` shares the name"));
                    }
                    if has_token(node, &["static"]) {
                        return Err(format!("static method `{name}` shares the name"));
                    }
                    owner_of(
                        parent
                            .and_then(|p| p.parent())
                            .expect("class body has a class"),
                    )
                }
                ("method_signature", Some("class_body")) => {
                    Err(format!("overload signature of `{name}`"))
                }
                ("abstract_method_signature", Some("class_body")) => owner_of(
                    parent
                        .and_then(|p| p.parent())
                        .expect("class body has a class"),
                ),
                ("method_signature", Some("interface_body" | "object_type")) => {
                    match parent.and_then(|p| p.parent()) {
                        Some(owner) if owner.kind() == "interface_declaration" => owner_of(owner),
                        _ => Err(format!(
                            "`{name}` is declared in a type literal, which has no name to resolve"
                        )),
                    }
                }
                ("method_definition", _) => Err(format!(
                    "object literal method `{name}` may implement the method structurally"
                )),
                _ => Err(format!(
                    "function `{name}` shares the name; calls by name cannot tell them apart"
                )),
            }
        }
        Family::Python => {
            let holder = node
                .parent()
                .filter(|p| p.kind() == "decorated_definition")
                .unwrap_or(node);
            if holder.kind() == "decorated_definition"
                && named_children(holder).iter().any(|d| {
                    d.kind() == "decorator"
                        && matches!(
                            text(*d, bytes).trim_start_matches('@').trim(),
                            "overload" | "typing.overload"
                        )
                })
            {
                return Err(format!("`@overload` declaration of `{name}`"));
            }
            match holder
                .parent()
                .filter(|p| p.kind() == "block")
                .and_then(|b| b.parent())
            {
                Some(class) if class.kind() == "class_definition" => owner_of(class),
                _ => Err(format!(
                    "function `{name}` shares the name; calls by name cannot tell them apart"
                )),
            }
        }
    }
}

fn has_token(node: Node<'_>, tokens: &[&str]) -> bool {
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .take_while(|c| c.kind() != "property_identifier")
        .any(|c| tokens.contains(&c.kind()));
    found
}

/// A declaration of `name` the family cannot read as a method.
fn opaque_declaration(
    node: Node<'_>,
    bytes: &[u8],
    family: Family,
    name: &str,
) -> Option<(usize, &'static str, String)> {
    let line = node.start_position().row + 1;
    match family {
        Family::Rust => {
            if node.kind() == "identifier"
                && text(node, bytes) == name
                && node.parent().is_some_and(|p| p.kind() == "token_tree")
                && node.prev_sibling().is_some_and(|p| text(p, bytes) == "fn")
            {
                return Some((
                    line,
                    "macro",
                    format!("`fn {name}` is written inside a macro, which cannot be rewritten"),
                ));
            }
            None
        }
        Family::TypeScript => {
            let parent = node.parent()?;
            let is_name =
                |field: &str| parent.child_by_field_name(field).map(|c| c.id()) == Some(node.id());
            let defines = match parent.kind() {
                "public_field_definition" | "property_signature" => is_name("name"),
                _ => false,
            };
            (defines
                && matches!(
                    node.kind(),
                    "property_identifier" | "private_property_identifier"
                )
                && text(node, bytes) == name)
                .then(|| {
                    (
                        line,
                        "property",
                        format!(
                            "property `{name}` may hold an implementation that cannot be rewritten"
                        ),
                    )
                })
        }
        Family::Python => {
            let left = node.child_by_field_name("left")?;
            let in_class_body = node
                .parent()
                .filter(|p| p.kind() == "expression_statement")
                .and_then(|p| p.parent())
                .filter(|p| p.kind() == "block")
                .and_then(|p| p.parent())
                .is_some_and(|p| p.kind() == "class_definition");
            (node.kind() == "assignment"
                && in_class_body
                && left.kind() == "identifier"
                && text(left, bytes) == name)
                .then(|| {
                    (
                        line,
                        "class_attribute",
                        format!(
                            "class attribute `{name}` may bind a callable that cannot be rewritten"
                        ),
                    )
                })
        }
    }
}

fn rust_use_leaves(node: Node<'_>, bytes: &[u8], root: Option<&str>, out: &mut Vec<RustUse>) {
    let root_of = |path: Option<Node<'_>>| -> String {
        root.map(str::to_string)
            .or_else(|| path.map(|p| leftmost_text(p, bytes)))
            .unwrap_or_default()
    };
    match node.kind() {
        "identifier" | "crate" | "self" | "super" => out.push(RustUse {
            bound: Some(text(node, bytes).to_string()),
            root: root_of(Some(node)),
        }),
        "scoped_identifier" => out.push(RustUse {
            bound: node
                .child_by_field_name("name")
                .map(|n| text(n, bytes).to_string()),
            root: root_of(node.child_by_field_name("path")),
        }),
        "use_as_clause" => out.push(RustUse {
            bound: node
                .child_by_field_name("alias")
                .map(|n| text(n, bytes).to_string()),
            root: root_of(node.child_by_field_name("path")),
        }),
        "use_wildcard" => out.push(RustUse {
            bound: None,
            root: root_of(node.named_child(0)),
        }),
        "scoped_use_list" => {
            let list_root = root_of(node.child_by_field_name("path"));
            if let Some(list) = node.child_by_field_name("list") {
                for child in named_children(list) {
                    rust_use_leaves(child, bytes, Some(&list_root), out);
                }
            }
        }
        "use_list" => {
            for child in named_children(node) {
                rust_use_leaves(child, bytes, root, out);
            }
        }
        _ => out.push(RustUse {
            bound: None,
            root: text(node, bytes).to_string(),
        }),
    }
}

fn ts_imports(node: Node<'_>, bytes: &[u8], out: &mut Vec<Import>) {
    let Some(source) = node.child_by_field_name("source") else {
        return;
    };
    let module = text(source, bytes)
        .trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .to_string();
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.kind() == "import_specifier" {
            let original = current.child_by_field_name("name").map(|n| text(n, bytes));
            let alias = current.child_by_field_name("alias").map(|n| text(n, bytes));
            if let Some(original) = original {
                out.push(Import {
                    local: alias.unwrap_or(original).to_string(),
                    module: module.clone(),
                    original: original.to_string(),
                });
            }
            continue;
        }
        stack.extend(named_children(current));
    }
}

fn python_imports(
    node: Node<'_>,
    bytes: &[u8],
    out: &mut Vec<Import>,
    neutral: &mut BTreeSet<String>,
) {
    let Some(module) = node.child_by_field_name("module_name") else {
        return;
    };
    let module = text(module, bytes).to_string();
    let mut cursor = node.walk();
    for name in node.children_by_field_name("name", &mut cursor) {
        let (original, local) = match name.kind() {
            "aliased_import" => (
                name.child_by_field_name("name").map(|n| text(n, bytes)),
                name.child_by_field_name("alias").map(|n| text(n, bytes)),
            ),
            _ => (Some(text(name, bytes)), None),
        };
        let Some(original) = original else { continue };
        let local = local.unwrap_or(original).to_string();
        if PYTHON_NEUTRAL_BASES
            .iter()
            .any(|(m, b)| *m == module && *b == original)
        {
            neutral.insert(local.clone());
        }
        out.push(Import {
            local,
            module: module.clone(),
            original: original.to_string(),
        });
    }
}
