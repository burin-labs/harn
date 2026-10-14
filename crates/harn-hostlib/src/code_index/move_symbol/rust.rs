//! Rust arm of `move_symbol`: crate-rooted module paths and use-trees.

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::path::PathBuf;

use tree_sitter::{Node, Tree};

use super::text::{self, Edit};
use super::{
    Binding, FileModel, ImportRequest, ListStatement, ModuleRef, QualifiedRewrite, Shape, Site,
    TopItem,
};

/// A Cargo package: its directory (workspace-relative, `""` at the root)
/// and the identifier other crates use for its library.
#[derive(Clone, Debug)]
struct CrateInfo {
    dir: String,
    ident: String,
    has_lib: bool,
}

/// Where one file sits: inside the library's module tree (`in_lib`), or in
/// a crate that reaches the library by its package name (tests, examples,
/// benches, binaries).
#[derive(Clone, Debug)]
struct FileCtx {
    krate: CrateInfo,
    in_lib: bool,
    module: Vec<String>,
}

pub(crate) struct Workspace {
    root: PathBuf,
    crates: HashMap<String, Option<CrateInfo>>,
}

const ITEM_KINDS: &[&str] = &[
    "function_item",
    "struct_item",
    "enum_item",
    "union_item",
    "trait_item",
    "type_item",
    "const_item",
    "static_item",
    "mod_item",
    "macro_definition",
];

const PATH_KINDS: &[&str] = &["scoped_identifier", "scoped_type_identifier"];

impl Workspace {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            crates: HashMap::new(),
        }
    }

    fn crate_at(&mut self, dir: &str) -> Option<CrateInfo> {
        if let Some(found) = self.crates.get(dir) {
            return found.clone();
        }
        let manifest = self.root.join(dir).join("Cargo.toml");
        let info = std::fs::read_to_string(&manifest)
            .ok()
            .and_then(|toml| parse_manifest(&toml))
            .map(|(package, lib_name, lib_path)| {
                let lib_rel = lib_path.unwrap_or_else(|| "src/lib.rs".to_string());
                CrateInfo {
                    dir: dir.to_string(),
                    ident: lib_name.unwrap_or(package).replace('-', "_"),
                    has_lib: self.root.join(dir).join(lib_rel).exists(),
                }
            });
        self.crates.insert(dir.to_string(), info.clone());
        info
    }

    fn file_ctx(&mut self, rel: &str) -> Option<FileCtx> {
        let parts: Vec<&str> = rel.split('/').collect();
        for depth in (0..parts.len()).rev() {
            let dir = parts[..depth].join("/");
            let Some(krate) = self.crate_at(&dir) else {
                continue;
            };
            let inner = &parts[depth..];
            let (in_lib, module) = match inner {
                ["src", "lib.rs"] => (true, Vec::new()),
                ["src", "main.rs"] => (!krate.has_lib, Vec::new()),
                ["src", "bin", ..] => (false, Vec::new()),
                ["src", rest @ ..] => {
                    let mut module: Vec<String> = rest.iter().map(|s| s.to_string()).collect();
                    let last = module.pop().unwrap_or_default();
                    let stem = last.strip_suffix(".rs").unwrap_or(&last).to_string();
                    if stem != "mod" {
                        module.push(stem);
                    }
                    (true, module)
                }
                _ => (false, Vec::new()),
            };
            return Some(FileCtx {
                krate,
                in_lib,
                module,
            });
        }
        None
    }

    pub(crate) fn module_of(&mut self, rel: &str) -> Option<ModuleRef> {
        let ctx = self.file_ctx(rel)?;
        ctx.in_lib.then_some(ModuleRef::Rust {
            crate_dir: ctx.krate.dir,
            path: ctx.module,
        })
    }

    /// Whether `module` names a module file of the library.
    fn module_exists(&self, krate: &CrateInfo, module: &[String]) -> bool {
        if module.is_empty() {
            return true;
        }
        let base = self
            .root
            .join(&krate.dir)
            .join("src")
            .join(module.join("/"));
        base.with_extension("rs").exists() || base.join("mod.rs").exists()
    }

    /// Resolve a path written in `module` of `ctx`'s file to a library path.
    /// `aliases` maps names a use-declaration binds, for expression paths.
    fn resolve(
        &self,
        ctx: &FileCtx,
        module: &[String],
        segs: &[String],
        aliases: Option<&HashMap<String, Vec<String>>>,
    ) -> Option<Vec<String>> {
        let first = segs.first()?;
        let rest = || segs[1..].to_vec();
        match first.as_str() {
            "::" => None,
            "crate" => ctx.in_lib.then(rest),
            "self" => ctx.in_lib.then(|| [module, &segs[1..]].concat()),
            "super" => {
                if !ctx.in_lib {
                    return None;
                }
                let ups = segs.iter().take_while(|s| *s == "super").count();
                (module.len() >= ups)
                    .then(|| [&module[..module.len() - ups], &segs[ups..]].concat())
            }
            name if !ctx.in_lib && name == ctx.krate.ident => Some(rest()),
            name => {
                if let Some(target) = aliases.and_then(|a| a.get(name)) {
                    return Some([target.as_slice(), &segs[1..]].concat());
                }
                let child = [module, &segs[..1]].concat();
                (ctx.in_lib && self.module_exists(&ctx.krate, &child))
                    .then(|| [module, segs].concat())
            }
        }
    }

    fn render_module(
        &self,
        ctx: &FileCtx,
        krate_dir: &str,
        path: &[String],
    ) -> Result<String, String> {
        if ctx.krate.dir != krate_dir {
            return Err(format!(
                "a file outside crate `{krate_dir}` cannot name its modules"
            ));
        }
        let head = if ctx.in_lib {
            "crate".to_string()
        } else if ctx.krate.has_lib {
            ctx.krate.ident.clone()
        } else {
            return Err("the crate has no library for other targets to import".into());
        };
        Ok(std::iter::once(head)
            .chain(path.iter().cloned())
            .collect::<Vec<_>>()
            .join("::"))
    }

    pub(crate) fn model(&mut self, rel: &str, tree: &Tree, source: &str) -> FileModel {
        let mut model = FileModel::default();
        let ctx = self.file_ctx(rel);
        let root = tree.root_node();
        let mut last_use_end: Option<usize> = None;
        let mut preamble_end = 0;
        let mut seen_item = false;
        for child in text::members(root) {
            match child.kind() {
                "use_declaration" => {
                    last_use_end = Some(child.end_byte());
                    seen_item = true;
                }
                "inner_attribute_item" if !seen_item => preamble_end = child.end_byte(),
                "line_comment" | "block_comment"
                    if !seen_item && text::text(child, source).starts_with("//!") =>
                {
                    preamble_end = child.end_byte();
                }
                kind if ITEM_KINDS.contains(&kind) => {
                    seen_item = true;
                    if let Some(name) = child.child_by_field_name("name") {
                        model.items.push(TopItem {
                            name: text::text(name, source).to_string(),
                            range: child.byte_range(),
                            exported: is_public(child, source),
                            type_like: false,
                            movable: kind != "mod_item" && kind != "macro_definition",
                        });
                    }
                }
                _ => seen_item = true,
            }
        }
        model.has_imports = last_use_end.is_some();
        model.insert_at = match last_use_end {
            Some(end) => text::next_line_start(source, end),
            None if preamble_end > 0 => text::next_line_start(source, preamble_end),
            None => 0,
        };
        if let Some(ctx) = ctx {
            let module = ctx.module.clone();
            self.collect_uses(&ctx, root, &module, true, source, &mut model);
        }
        model
    }

    fn collect_uses(
        &self,
        ctx: &FileCtx,
        node: Node<'_>,
        module: &[String],
        top_level: bool,
        source: &str,
        model: &mut FileModel,
    ) {
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        for child in children {
            match child.kind() {
                "use_declaration" => self.add_use(ctx, child, module, top_level, source, model),
                "mod_item" => {
                    if let (Some(name), Some(body)) = (
                        child.child_by_field_name("name"),
                        child.child_by_field_name("body"),
                    ) {
                        let inner = [module, &[text::text(name, source).to_string()]].concat();
                        self.collect_uses(ctx, body, &inner, false, source, model);
                    }
                }
                kind if kind.contains("comment") || kind.contains("string") => {}
                _ => self.collect_uses(ctx, child, module, false, source, model),
            }
        }
    }

    fn add_use(
        &self,
        ctx: &FileCtx,
        decl: Node<'_>,
        module: &[String],
        top_level: bool,
        source: &str,
        model: &mut FileModel,
    ) {
        model.import_ranges.push(decl.byte_range());
        let Some(argument) = decl.child_by_field_name("argument") else {
            return;
        };
        let prefix = visibility(decl, source)
            .map(|v| format!("{v} "))
            .unwrap_or_default();
        let mut leaves = Vec::new();
        flatten(argument, &[], source, &mut leaves);
        for leaf in leaves {
            let resolved = self.resolve(ctx, module, &leaf.segs, None);
            let (target, imported) = if leaf.glob {
                let target = match resolved {
                    Some(path) => ModuleRef::Rust {
                        crate_dir: ctx.krate.dir.clone(),
                        path,
                    },
                    None => ModuleRef::External(leaf.segs.join("::")),
                };
                (target, String::new())
            } else {
                let Some((last, parent)) = leaf.segs.split_last() else {
                    continue;
                };
                let target = match resolved {
                    Some(path) if !path.is_empty() => ModuleRef::Rust {
                        crate_dir: ctx.krate.dir.clone(),
                        path: path[..path.len() - 1].to_vec(),
                    },
                    _ => ModuleRef::External(parent.join("::")),
                };
                (target, last.clone())
            };
            let removal = leaf_removal(leaf.node, decl);
            let sole = removal == decl.byte_range();
            model.bindings.push(Binding {
                local: leaf.alias.clone().unwrap_or_else(|| imported.clone()),
                imported,
                target,
                shape: if leaf.glob { Shape::Glob } else { Shape::Named },
                type_only: false,
                reexport: !prefix.is_empty(),
                prefix: prefix.clone(),
                top_level,
                removal,
                statement: decl.byte_range(),
                sole,
            });
        }
        if top_level && argument.kind() == "scoped_use_list" {
            let list = argument.child_by_field_name("list");
            let path = argument.child_by_field_name("path");
            if let (Some(list), Some(path)) = (list, path) {
                let members = text::members(list);
                let segs = path_segments(path, source);
                if let (Some(last), Some(segs)) = (members.last(), segs) {
                    let target = match self.resolve(ctx, module, &segs, None) {
                        Some(path) => ModuleRef::Rust {
                            crate_dir: ctx.krate.dir.clone(),
                            path,
                        },
                        None => ModuleRef::External(segs.join("::")),
                    };
                    model.lists.push(ListStatement {
                        target,
                        type_only: false,
                        reexport: !prefix.is_empty(),
                        prefix,
                        append_at: last.end_byte(),
                    });
                }
            }
        }
    }

    pub(crate) fn render(&mut self, rel: &str, req: &ImportRequest) -> Result<String, String> {
        let ctx = self
            .file_ctx(rel)
            .ok_or_else(|| format!("`{rel}` is not inside a Cargo package"))?;
        let module = match &req.target {
            ModuleRef::Rust { crate_dir, path } => self.render_module(&ctx, crate_dir, path)?,
            ModuleRef::External(text) => text.clone(),
            other => return Err(format!("not a Rust module: {other:?}")),
        };
        let leaf = if req.shape == Shape::Glob {
            "*".to_string()
        } else if req.local != req.imported {
            format!("{} as {}", req.imported, req.local)
        } else {
            req.imported.clone()
        };
        let path = if module.is_empty() {
            leaf
        } else {
            format!("{module}::{leaf}")
        };
        Ok(format!("{}use {path};", req.prefix))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn qualified(
        &mut self,
        rel: &str,
        source: &str,
        model: &FileModel,
        node: Node<'_>,
        from: &ModuleRef,
        to: &ModuleRef,
        bare: bool,
    ) -> Result<Option<QualifiedRewrite>, String> {
        let Some((path, segs)) = qualifier_of(node, source) else {
            return Ok(None);
        };
        let Some(ctx) = self.file_ctx(rel) else {
            return Ok(None);
        };
        let module = enclosing_module(&ctx.module, node, source);
        let aliases = alias_paths(model);
        let resolved = self.resolve(&ctx, &module, &segs, Some(&aliases));
        let ModuleRef::Rust {
            crate_dir,
            path: from_path,
        } = from
        else {
            return Ok(None);
        };
        if resolved.as_deref() != Some(from_path.as_slice()) {
            return Ok(None);
        }
        let edit = if bare {
            Edit::replace(path.start..node.start_byte(), "")
        } else {
            let ModuleRef::Rust { path: to_path, .. } = to else {
                return Ok(None);
            };
            Edit::replace(path, self.render_module(&ctx, crate_dir, to_path)?)
        };
        Ok(Some(QualifiedRewrite { edit, import: None }))
    }

    /// Private fields and methods of the source module the moved item reads.
    pub(crate) fn private_members_used(
        &self,
        rel: &str,
        source: &str,
        tree: &Tree,
        members: &BTreeSet<String>,
    ) -> Vec<Site> {
        let mut sites = Vec::new();
        for child in text::members(tree.root_node()) {
            let candidates: Vec<Node<'_>> = match child.kind() {
                "struct_item" => child
                    .child_by_field_name("body")
                    .map(|body| {
                        text::members(body)
                            .into_iter()
                            .filter(|f| f.kind() == "field_declaration")
                            .collect()
                    })
                    .unwrap_or_default(),
                "impl_item" if child.child_by_field_name("trait").is_none() => child
                    .child_by_field_name("body")
                    .map(|body| {
                        text::members(body)
                            .into_iter()
                            .filter(|f| f.kind() == "function_item")
                            .collect()
                    })
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            for member in candidates {
                let Some(name) = member.child_by_field_name("name") else {
                    continue;
                };
                let name = text::text(name, source);
                if members.contains(name) && !is_public(member, source) {
                    sites.push(Site::at(
                        rel,
                        source,
                        member.start_byte(),
                        "declaration",
                        format!(
                            "`{name}` is private to the source module but the moved item uses it"
                        ),
                    ));
                }
            }
        }
        sites
    }

    /// `self::`/`super::` paths inside the moved item, rewritten to paths
    /// that still resolve from the destination.
    pub(crate) fn relative_paths_in(
        &mut self,
        src_rel: &str,
        dest_rel: &str,
        source: &str,
        item: Node<'_>,
    ) -> Result<Vec<(Range<usize>, String)>, String> {
        let (Some(src_ctx), Some(dest_ctx)) = (self.file_ctx(src_rel), self.file_ctx(dest_rel))
        else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        let mut failure = None;
        text::walk(item, |node| {
            if node.kind() == "use_declaration" {
                return false;
            }
            if !PATH_KINDS.contains(&node.kind()) {
                return true;
            }
            let Some(segs) = path_segments(node, source) else {
                return false;
            };
            if !matches!(segs.first().map(String::as_str), Some("self" | "super")) {
                return false;
            }
            let module = enclosing_module(&src_ctx.module, node, source);
            match self.resolve(&src_ctx, &module, &segs, None) {
                Some(abs) if !abs.is_empty() => {
                    match self.render_module(&dest_ctx, &src_ctx.krate.dir, &abs[..abs.len() - 1]) {
                        Ok(head) => {
                            out.push((
                                node.byte_range(),
                                format!("{head}::{}", abs[abs.len() - 1]),
                            ));
                        }
                        Err(err) => failure = Some(err),
                    }
                }
                _ => failure = Some(format!("cannot resolve `{}`", segs.join("::"))),
            }
            false
        });
        match failure {
            Some(err) => Err(err),
            None => Ok(out),
        }
    }

    /// The parent module file of a new destination and the `mod` line it
    /// needs.
    pub(crate) fn module_declaration(
        &mut self,
        dest_rel: &str,
        public: bool,
    ) -> Result<(String, String), String> {
        let ctx = self
            .file_ctx(dest_rel)
            .filter(|ctx| ctx.in_lib)
            .ok_or_else(|| format!("`{dest_rel}` is not under a library's `src/`"))?;
        let (name, parent) = ctx
            .module
            .split_last()
            .ok_or_else(|| format!("`{dest_rel}` is a crate root"))?;
        let src = if ctx.krate.dir.is_empty() {
            "src".to_string()
        } else {
            format!("{}/src", ctx.krate.dir)
        };
        let parent_file = if parent.is_empty() {
            if ctx.krate.has_lib {
                format!("{src}/lib.rs")
            } else {
                format!("{src}/main.rs")
            }
        } else {
            let base = format!("{src}/{}", parent.join("/"));
            if self.root.join(format!("{base}.rs")).exists() {
                format!("{base}.rs")
            } else {
                format!("{base}/mod.rs")
            }
        };
        if !self.root.join(&parent_file).exists() {
            return Err(format!(
                "parent module file `{parent_file}` for `{dest_rel}` does not exist"
            ));
        }
        let visibility = if public { "pub " } else { "" };
        Ok((parent_file, format!("{visibility}mod {name};")))
    }
}

/// Insert a `mod` declaration after the parent's last `mod x;` (or use).
pub(crate) fn module_declaration_edit(source: &str, tree: &Tree, line: &str) -> Edit {
    let children = text::members(tree.root_node());
    let anchor = children
        .iter()
        .rev()
        .find(|n| n.kind() == "mod_item" && n.child_by_field_name("body").is_none())
        .or_else(|| {
            children
                .iter()
                .rev()
                .find(|n| n.kind() == "use_declaration")
        });
    match anchor {
        Some(node) => Edit::insert(
            text::next_line_start(source, node.end_byte()),
            format!("{line}\n"),
            0,
        ),
        None if source.trim().is_empty() => Edit::insert(0, format!("{line}\n"), 0),
        None => Edit::insert(0, format!("{line}\n\n"), 0),
    }
}

/// Names the moved item uses (first path segments, types, values) and the
/// fields and methods it reads.
pub(crate) fn free_names(
    item: Node<'_>,
    source: &str,
    own: &str,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut names = BTreeSet::new();
    let mut members = BTreeSet::new();
    text::walk(item, |node| {
        let kind = node.kind();
        if kind == "use_declaration" {
            return false;
        }
        let parent = node.parent();
        match kind {
            "identifier" | "type_identifier" => {
                let is_tail = parent.is_some_and(|p| {
                    PATH_KINDS.contains(&p.kind()) && text::is_field_child(p, "name", node)
                }) || node.prev_sibling().is_some_and(|prev| prev.kind() == "::");
                if !is_tail {
                    names.insert(text::text(node, source).to_string());
                }
            }
            "field_identifier" | "shorthand_field_identifier"
                if parent.is_some_and(|p| p.kind() != "field_declaration") =>
            {
                members.insert(text::text(node, source).to_string());
            }
            _ => {}
        }
        true
    });
    names.remove(own);
    (names, members)
}

fn is_public(node: Node<'_>, source: &str) -> bool {
    visibility(node, source)
        .is_some_and(|v| v.starts_with("pub") && v.replace(' ', "") != "pub(self)")
}

fn visibility<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .find(|c| c.kind() == "visibility_modifier")
        .map(|c| text::text(c, source));
    found
}

/// The qualifying path in front of `node` and its segments: the `path` of
/// a scoped identifier, or the `a::b::` tokens before it inside a macro's
/// token tree, which tree-sitter leaves unparsed.
fn qualifier_of(node: Node<'_>, source: &str) -> Option<(Range<usize>, Vec<String>)> {
    let parent = node.parent()?;
    if PATH_KINDS.contains(&parent.kind()) && text::is_field_child(parent, "name", node) {
        let path = parent.child_by_field_name("path")?;
        return Some((path.byte_range(), path_segments(path, source)?));
    }
    if parent.kind() != "token_tree" {
        return None;
    }
    let mut segs = Vec::new();
    let mut start = None;
    let mut cursor = node.prev_sibling();
    while let Some(sep) = cursor.filter(|n| n.kind() == "::") {
        let seg = sep.prev_sibling()?;
        let text = text::text(seg, source);
        if !super::super::refactor_core::is_identifier_token(text) {
            return None;
        }
        segs.push(text.to_string());
        start = Some(seg.start_byte());
        cursor = seg.prev_sibling();
    }
    segs.reverse();
    let end = node.prev_sibling()?.start_byte();
    Some((start?..end, segs))
}

/// Module path of the innermost inline `mod` around `node`.
fn enclosing_module(file_module: &[String], node: Node<'_>, source: &str) -> Vec<String> {
    let mut inline = Vec::new();
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "mod_item" {
            if let Some(name) = n.child_by_field_name("name") {
                inline.push(text::text(name, source).to_string());
            }
        }
        current = n.parent();
    }
    inline.reverse();
    [file_module, &inline].concat()
}

/// Library paths of names that use-declarations bind, for qualifier lookup.
fn alias_paths(model: &FileModel) -> HashMap<String, Vec<String>> {
    model
        .bindings
        .iter()
        .filter(|b| b.shape == Shape::Named)
        .filter_map(|b| match &b.target {
            ModuleRef::Rust { path, .. } => Some((
                b.local.clone(),
                [path.as_slice(), std::slice::from_ref(&b.imported)].concat(),
            )),
            _ => None,
        })
        .collect()
}

pub(crate) fn path_segments(node: Node<'_>, source: &str) -> Option<Vec<String>> {
    match node.kind() {
        "scoped_identifier" | "scoped_type_identifier" => {
            let mut segs = match node.child_by_field_name("path") {
                Some(path) => path_segments(path, source)?,
                None if text::text(node, source).starts_with("::") => vec!["::".to_string()],
                None => Vec::new(),
            };
            segs.push(text::text(node.child_by_field_name("name")?, source).to_string());
            Some(segs)
        }
        "identifier" | "type_identifier" | "crate" | "self" | "super" => {
            Some(vec![text::text(node, source).to_string()])
        }
        _ => None,
    }
}

struct Leaf<'tree> {
    segs: Vec<String>,
    alias: Option<String>,
    glob: bool,
    node: Node<'tree>,
}

fn flatten<'tree>(node: Node<'tree>, prefix: &[String], source: &str, out: &mut Vec<Leaf<'tree>>) {
    match node.kind() {
        "use_as_clause" => {
            let segs = node
                .child_by_field_name("path")
                .and_then(|p| path_segments(p, source))
                .unwrap_or_default();
            out.push(Leaf {
                segs: [prefix, &segs].concat(),
                alias: node
                    .child_by_field_name("alias")
                    .map(|a| text::text(a, source).to_string()),
                glob: false,
                node,
            });
        }
        "use_wildcard" => {
            let segs = text::members(node)
                .first()
                .and_then(|p| path_segments(*p, source))
                .unwrap_or_default();
            out.push(Leaf {
                segs: [prefix, &segs].concat(),
                alias: None,
                glob: true,
                node,
            });
        }
        "scoped_use_list" => {
            let segs = node
                .child_by_field_name("path")
                .and_then(|p| path_segments(p, source))
                .unwrap_or_default();
            let inner = [prefix, &segs].concat();
            if let Some(list) = node.child_by_field_name("list") {
                for member in text::members(list) {
                    flatten(member, &inner, source, out);
                }
            }
        }
        "use_list" => {
            for member in text::members(node) {
                flatten(member, prefix, source, out);
            }
        }
        "self" if !prefix.is_empty() => out.push(Leaf {
            segs: prefix.to_vec(),
            alias: None,
            glob: false,
            node,
        }),
        _ => {
            if let Some(segs) = path_segments(node, source) {
                out.push(Leaf {
                    segs: [prefix, &segs].concat(),
                    alias: None,
                    glob: false,
                    node,
                });
            }
        }
    }
}

/// Bytes that remove one use-tree leaf: the element and one comma, or the
/// enclosing group (recursively) when it is the group's only element, or
/// the whole declaration.
fn leaf_removal(leaf: Node<'_>, decl: Node<'_>) -> Range<usize> {
    let mut node = leaf;
    loop {
        let Some(parent) = node.parent() else {
            return decl.byte_range();
        };
        if parent.kind() != "use_list" {
            return decl.byte_range();
        }
        let elements = text::members(parent);
        if elements.len() > 1 {
            let ranges: Vec<Range<usize>> = elements.iter().map(|e| e.byte_range()).collect();
            let index = elements
                .iter()
                .position(|e| e.id() == node.id())
                .unwrap_or(0);
            return text::list_element_removal(&ranges, index);
        }
        match parent.parent() {
            Some(owner) if owner.kind() == "scoped_use_list" => node = owner,
            _ => return decl.byte_range(),
        }
    }
}

/// `(package name, [lib] name, [lib] path)` from a manifest, or `None`
/// when it declares no package (a virtual workspace root).
fn parse_manifest(toml: &str) -> Option<(String, Option<String>, Option<String>)> {
    let mut section = "";
    let mut package = None;
    let mut lib_name = None;
    let mut lib_path = None;
    for line in toml.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line;
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').to_string();
        match (section, key.trim()) {
            ("[package]", "name") => package = Some(value),
            ("[lib]", "name") => lib_name = Some(value),
            ("[lib]", "path") => lib_path = Some(value),
            _ => {}
        }
    }
    package.map(|p| (p, lib_name, lib_path))
}
