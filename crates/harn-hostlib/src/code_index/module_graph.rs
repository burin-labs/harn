//! `code_index.module_graph` — the workspace's dependency graph rolled
//! up from files to directories or modules.
//!
//! The file-level graph answers "what does this file import?". An
//! architecture question is one level up: "does `ui` depend on
//! `storage`?". This builtin answers it over the whole workspace in one
//! call, which Cypher cannot do because it caps result rows.
//!
//! Edges come from explicit import statements only. Same-module
//! visibility ([`IndexState::imports_of`] folds it in for Swift targets
//! and Go packages) is deliberately left out: every file of a module
//! sees every other one, so at module granularity it is nothing but a
//! self-loop, and at directory granularity it would invent edges no
//! source line declares.
//!
//! Nodes are the files of languages with an import rule
//! (`data/code_index_import_rules.json`). Documentation, config, and
//! languages the index cannot read imports from are not nodes.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use harn_vm::VmValue;

use super::builtins::SharedIndex;
use super::file_table::FileId;
use super::imports;
use super::state::IndexState;
use crate::error::HostlibError;
use crate::tools::args::{
    build_dict, dict_arg, optional_bool, optional_int, optional_string, optional_string_list,
    str_value,
};

pub(super) const BUILTIN: &str = "hostlib_code_index_module_graph";

/// Sample file pairs kept per edge. Enough to show a person where an
/// edge comes from without returning every import in a large module.
const MAX_SAMPLES: usize = 3;

/// The node id for files at the workspace root.
const ROOT_ID: &str = ".";

/// Files whose directory is a package root in the language that owns
/// them. Module granularity groups a file under the nearest one when
/// the index knows no finer module for it.
const PACKAGE_MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "setup.py",
    "Package.swift",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "mix.exs",
    "composer.json",
    "build.zig",
    "harn.toml",
];

/// How files are grouped into nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Granularity {
    /// The file's directory.
    Dir,
    /// The module the file belongs to: a Swift target or Go package when
    /// the index knows one, else the nearest directory holding a package
    /// manifest, else the file's directory.
    Module,
}

/// Validated request.
#[derive(Debug, Clone)]
pub(super) struct Options {
    pub granularity: Granularity,
    /// Keep at most this many leading path segments of a node id.
    pub depth: Option<usize>,
    /// Workspace-relative path prefixes. Files outside them are not
    /// nodes, and edges into them are dropped.
    pub roots: Vec<String>,
    pub include_external: bool,
    pub min_weight: u64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            granularity: Granularity::Module,
            depth: None,
            roots: Vec::new(),
            include_external: false,
            min_weight: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Node {
    pub id: String,
    pub files: u64,
    pub languages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Edge {
    pub from: String,
    pub to: String,
    /// Distinct import links from files in `from` into `to`. A file
    /// importing another file counts once; a file importing a whole
    /// module counts once per node that module's files fall in.
    pub weight: u64,
    /// Up to [`MAX_SAMPLES`] `(importing file, imported file or module
    /// root)` pairs, smallest first.
    pub sample: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct External {
    pub from: String,
    pub spec: String,
    /// Files in `from` with this unresolved import.
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ModuleGraph {
    pub indexed: bool,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub external: Vec<External>,
    pub unresolved_count: u64,
    pub index_seq: u64,
}

pub(super) fn run(index: &SharedIndex, args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let options = parse_options(args)?;
    let guard = index.lock().expect("code_index mutex poisoned");
    let graph = match guard.as_ref() {
        Some(state) => build(state, &options),
        None => ModuleGraph {
            indexed: false,
            nodes: Vec::new(),
            edges: Vec::new(),
            external: Vec::new(),
            unresolved_count: 0,
            index_seq: 0,
        },
    };
    drop(guard);
    Ok(graph_to_vm(&graph))
}

fn parse_options(args: &[VmValue]) -> Result<Options, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();
    let granularity = match optional_string(BUILTIN, dict, "granularity")?.as_deref() {
        None | Some("module") => Granularity::Module,
        Some("dir") => Granularity::Dir,
        Some(other) => {
            return Err(HostlibError::InvalidParameter {
                builtin: BUILTIN,
                param: "granularity",
                message: format!("must be \"dir\" or \"module\", got {other:?}"),
            })
        }
    };
    let depth = match dict.get("depth") {
        None | Some(VmValue::Nil) => None,
        Some(_) => {
            let value = optional_int(BUILTIN, dict, "depth", 0)?;
            if value < 1 {
                return Err(HostlibError::InvalidParameter {
                    builtin: BUILTIN,
                    param: "depth",
                    message: format!("must be >= 1, got {value}"),
                });
            }
            Some(usize::try_from(value).unwrap_or(usize::MAX))
        }
    };
    let min_weight = optional_int(BUILTIN, dict, "min_weight", 1)?;
    if min_weight < 1 {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "min_weight",
            message: format!("must be >= 1, got {min_weight}"),
        });
    }
    let roots = optional_string_list(BUILTIN, dict, "roots")?
        .into_iter()
        .map(|root| normalize_root(&root))
        .collect();
    Ok(Options {
        granularity,
        depth,
        roots,
        include_external: optional_bool(BUILTIN, dict, "include_external", false)?,
        min_weight: min_weight as u64,
    })
}

fn normalize_root(raw: &str) -> String {
    let trimmed = raw.trim().trim_start_matches("./").trim_end_matches('/');
    if trimmed == "." {
        String::new()
    } else {
        trimmed.to_string()
    }
}

fn in_roots(path: &str, roots: &[String]) -> bool {
    roots.is_empty()
        || roots.iter().any(|root| {
            root.is_empty()
                || path == root
                || path
                    .strip_prefix(root.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        })
}

/// Running weight and smallest sample pairs for one node-to-node edge.
type EdgeAcc = (u64, BTreeSet<(String, String)>);

/// Build the rolled-up graph. Pure over `state`; the caller holds the
/// index lock.
pub(super) fn build(state: &IndexState, options: &Options) -> ModuleGraph {
    let mut ids: Vec<FileId> = state
        .files
        .values()
        .filter(|file| imports::declares_import_rules(&file.language))
        .filter(|file| in_roots(&file.relative_path, &options.roots))
        .map(|file| file.id)
        .collect();
    ids.sort_unstable();

    let mut node_of: HashMap<FileId, String> = HashMap::with_capacity(ids.len());
    let mut nodes: BTreeMap<String, (u64, BTreeSet<String>)> = BTreeMap::new();
    for id in &ids {
        let file = &state.files[id];
        let node = node_id(state, *id, &file.relative_path, options);
        let entry = nodes.entry(node.clone()).or_default();
        entry.0 += 1;
        entry.1.insert(file.language.clone());
        node_of.insert(*id, node);
    }

    let mut edges: BTreeMap<(String, String), EdgeAcc> = BTreeMap::new();
    let mut add_edge = |from: &str, to: &str, pair: (String, String)| {
        if from == to {
            return;
        }
        let entry = edges.entry((from.to_string(), to.to_string())).or_default();
        entry.0 += 1;
        entry.1.insert(pair);
        if entry.1.len() > MAX_SAMPLES {
            entry.1.pop_last();
        }
    };
    // A module root expands to the same nodes for every importer, so
    // expand each one once.
    let mut root_nodes: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    let mut external: BTreeMap<(String, String), u64> = BTreeMap::new();
    let mut unresolved_count: u64 = 0;

    for id in &ids {
        let file = &state.files[id];
        let from = &node_of[id];
        for target in state.deps.imports_of(*id) {
            let (Some(to), Some(target_file)) = (node_of.get(&target), state.files.get(&target))
            else {
                continue;
            };
            add_edge(
                from,
                to,
                (
                    file.relative_path.clone(),
                    target_file.relative_path.clone(),
                ),
            );
        }
        for root in state.deps.module_imports_of(*id) {
            let targets = root_nodes.entry(root.as_str()).or_insert_with(|| {
                state
                    .modules
                    .files_in_roots(std::slice::from_ref(root))
                    .iter()
                    .filter_map(|file| node_of.get(file).map(String::as_str))
                    .collect()
            });
            for to in targets.iter() {
                add_edge(
                    from,
                    to,
                    (file.relative_path.clone(), display_root(root).to_string()),
                );
            }
        }
        let unresolved = state.deps.unresolved_imports(*id);
        unresolved_count += unresolved.len() as u64;
        if options.include_external {
            let specs: BTreeSet<String> = unresolved.iter().map(|raw| external_spec(raw)).collect();
            for spec in specs {
                *external.entry((from.clone(), spec)).or_default() += 1;
            }
        }
    }

    ModuleGraph {
        indexed: true,
        nodes: nodes
            .into_iter()
            .map(|(id, (files, languages))| Node {
                id,
                files,
                languages: languages.into_iter().collect(),
            })
            .collect(),
        edges: edges
            .into_iter()
            .filter(|(_, (weight, _))| *weight >= options.min_weight)
            .map(|((from, to), (weight, sample))| Edge {
                from,
                to,
                weight,
                sample: sample.into_iter().collect(),
            })
            .collect(),
        external: external
            .into_iter()
            .map(|((from, spec), count)| External { from, spec, count })
            .collect(),
        unresolved_count,
        index_seq: state.versions.current_seq,
    }
}

fn node_id(state: &IndexState, id: FileId, path: &str, options: &Options) -> String {
    let base = match options.granularity {
        Granularity::Dir => parent_dir(path).to_string(),
        Granularity::Module => match state.modules.home_of(id) {
            Some(home) => home.to_string(),
            None => manifest_dir(state, path).unwrap_or_else(|| parent_dir(path).to_string()),
        },
    };
    let truncated = match options.depth {
        Some(depth) => base.split('/').take(depth).collect::<Vec<_>>().join("/"),
        None => base,
    };
    if truncated.is_empty() {
        ROOT_ID.to_string()
    } else {
        truncated
    }
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("")
}

/// The nearest ancestor directory of `path` holding an indexed package
/// manifest.
fn manifest_dir(state: &IndexState, path: &str) -> Option<String> {
    let mut dir = parent_dir(path);
    loop {
        let found = PACKAGE_MANIFESTS.iter().any(|name| {
            let candidate = if dir.is_empty() {
                (*name).to_string()
            } else {
                format!("{dir}/{name}")
            };
            state.path_to_id.contains_key(&candidate)
        });
        if found {
            return Some(dir.to_string());
        }
        if dir.is_empty() {
            return None;
        }
        dir = parent_dir(dir);
    }
}

fn display_root(root: &str) -> &str {
    if root.is_empty() {
        ROOT_ID
    } else {
        root
    }
}

/// The name an unresolved import refers to: its string literal when it
/// has one (`import x from "react"` gives `react`), else the statement
/// as written without a trailing semicolon.
fn external_spec(raw: &str) -> String {
    imports::extract_string_literal(raw)
        .unwrap_or_else(|| raw.trim().trim_end_matches(';').trim().to_string())
}

fn graph_to_vm(graph: &ModuleGraph) -> VmValue {
    let list = |items: Vec<VmValue>| VmValue::List(Arc::new(items));
    build_dict([
        ("indexed", VmValue::Bool(graph.indexed)),
        (
            "nodes",
            list(
                graph
                    .nodes
                    .iter()
                    .map(|node| {
                        build_dict([
                            ("id", str_value(&node.id)),
                            ("files", VmValue::Int(node.files as i64)),
                            (
                                "languages",
                                list(node.languages.iter().map(str_value).collect()),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "edges",
            list(
                graph
                    .edges
                    .iter()
                    .map(|edge| {
                        build_dict([
                            ("from", str_value(&edge.from)),
                            ("to", str_value(&edge.to)),
                            ("weight", VmValue::Int(edge.weight as i64)),
                            (
                                "sample",
                                list(
                                    edge.sample
                                        .iter()
                                        .map(|(src, dst)| {
                                            list(vec![str_value(src), str_value(dst)])
                                        })
                                        .collect(),
                                ),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "external",
            list(
                graph
                    .external
                    .iter()
                    .map(|ext| {
                        build_dict([
                            ("from", str_value(&ext.from)),
                            ("spec", str_value(&ext.spec)),
                            ("count", VmValue::Int(ext.count as i64)),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "unresolved_count",
            VmValue::Int(graph.unresolved_count as i64),
        ),
        ("index_seq", VmValue::Int(graph.index_seq as i64)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    /// TypeScript relative imports, a Rust crate's modules, and two
    /// Swift targets, each with an intra-unit import that must roll up
    /// into a self-loop and disappear.
    fn fixture(root: &Path) {
        write(root, "web/package.json", "{}\n");
        write(
            root,
            "web/src/ui/App.ts",
            "import { load } from \"../data/store\";\nimport { fmt } from \"./fmt\";\nimport React from \"react\";\n",
        );
        write(
            root,
            "web/src/ui/fmt.ts",
            "import React from \"react\";\nexport const fmt = 1;\n",
        );
        write(root, "web/src/data/store.ts", "export function load() {}\n");

        write(root, "engine/Cargo.toml", "[package]\nname = \"engine\"\n");
        write(
            root,
            "engine/src/lib.rs",
            "use crate::core::run::start;\nuse std::collections::HashMap;\n",
        );
        write(
            root,
            "engine/src/core/run.rs",
            "use crate::util::helper;\npub fn start() {}\n",
        );
        write(root, "engine/src/util.rs", "pub fn helper() {}\n");

        write(root, "Package.swift", "// swift-tools-version:5.9\n");
        write(root, "Sources/Core/A.swift", "struct A {}\n");
        write(
            root,
            "Sources/Core/Net/B.swift",
            "import Foundation\nstruct B {}\n",
        );
        write(root, "Sources/App/Main.swift", "import Core\nlet a = A()\n");

        write(root, "README.md", "# docs are not nodes\n");
    }

    fn graph(options: Options) -> ModuleGraph {
        let dir = tempdir().unwrap();
        fixture(dir.path());
        let (state, _) = IndexState::build_from_root(dir.path());
        build(&state, &options)
    }

    fn edge(from: &str, to: &str, weight: u64) -> (String, String, u64) {
        (from.to_string(), to.to_string(), weight)
    }

    fn edge_keys(graph: &ModuleGraph) -> Vec<(String, String, u64)> {
        graph
            .edges
            .iter()
            .map(|e| (e.from.clone(), e.to.clone(), e.weight))
            .collect()
    }

    #[test]
    fn module_granularity_rolls_up_and_drops_self_loops() {
        let g = graph(Options {
            include_external: true,
            ..Options::default()
        });
        let ids: Vec<&str> = g.nodes.iter().map(|n| n.id.as_str()).collect();
        // TypeScript and Rust group under their package manifests, Swift
        // under its targets, and `Package.swift` under the root manifest.
        // Markdown is not a node.
        assert_eq!(ids, [".", "Sources/App", "Sources/Core", "engine", "web"]);
        let core = g.nodes.iter().find(|n| n.id == "Sources/Core").unwrap();
        assert_eq!(core.files, 2);
        assert_eq!(core.languages, ["swift"]);

        // Only the cross-module Swift import survives. Every TypeScript
        // and Rust import stays inside its package and is a self-loop,
        // although several cross directories (see the dir test).
        assert_eq!(edge_keys(&g), [edge("Sources/App", "Sources/Core", 1)]);
        assert_eq!(
            g.edges[0].sample,
            [(
                "Sources/App/Main.swift".to_string(),
                "Sources/Core".to_string()
            )]
        );
        assert!(g.indexed);
    }

    #[test]
    fn dir_granularity_keeps_cross_directory_edges() {
        let g = graph(Options {
            granularity: Granularity::Dir,
            ..Options::default()
        });
        // `import Core` reaches both directories of the Core target, once
        // each. Rust resolves through the crate root in both directions.
        // The same-directory `./fmt` import is a self-loop.
        assert_eq!(
            edge_keys(&g),
            [
                edge("Sources/App", "Sources/Core", 1),
                edge("Sources/App", "Sources/Core/Net", 1),
                edge("engine/src", "engine/src/core", 1),
                edge("engine/src/core", "engine/src", 1),
                edge("web/src/ui", "web/src/data", 1),
            ]
        );
        let ui = g.edges.iter().find(|e| e.from == "web/src/ui").unwrap();
        assert_eq!(
            ui.sample,
            [(
                "web/src/ui/App.ts".to_string(),
                "web/src/data/store.ts".to_string()
            )]
        );
    }

    #[test]
    fn depth_truncates_and_merges_nodes() {
        let g = graph(Options {
            granularity: Granularity::Dir,
            depth: Some(1),
            ..Options::default()
        });
        let ids: Vec<&str> = g.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, [".", "Sources", "engine", "web"]);
        // Every edge is now inside one top-level directory.
        assert!(g.edges.is_empty(), "{:?}", g.edges);
    }

    #[test]
    fn external_imports_are_counted_per_node_and_file() {
        let g = graph(Options {
            include_external: true,
            ..Options::default()
        });
        let ext: Vec<(&str, &str, u64)> = g
            .external
            .iter()
            .map(|e| (e.from.as_str(), e.spec.as_str(), e.count))
            .collect();
        assert_eq!(
            ext,
            [
                ("Sources/Core", "import Foundation", 1),
                ("engine", "use std::collections::HashMap", 1),
                // Two files import react: counted per file, not per line.
                ("web", "react", 2),
            ]
        );
        assert_eq!(g.unresolved_count, 4);

        let without = graph(Options::default());
        assert!(without.external.is_empty());
        assert_eq!(without.unresolved_count, 4);
    }

    #[test]
    fn roots_and_min_weight_filter() {
        let g = graph(Options {
            granularity: Granularity::Dir,
            roots: vec!["web".into()],
            ..Options::default()
        });
        assert!(g.nodes.iter().all(|n| n.id.starts_with("web")));
        assert_eq!(edge_keys(&g), [edge("web/src/ui", "web/src/data", 1)]);

        let heavy = graph(Options {
            granularity: Granularity::Dir,
            min_weight: 2,
            ..Options::default()
        });
        assert!(heavy.edges.is_empty());
        // An edge filter leaves every node in place.
        let all = graph(Options {
            granularity: Granularity::Dir,
            ..Options::default()
        });
        assert_eq!(heavy.nodes, all.nodes);
    }

    #[test]
    fn output_is_deterministic_across_independent_builds() {
        let options = Options {
            granularity: Granularity::Dir,
            include_external: true,
            ..Options::default()
        };
        // Two separate indexes assign file ids in walk order and hash
        // into different HashMap layouts; the answer must not move.
        let a = graph(options.clone());
        let b = graph(options);
        assert_eq!(a.nodes, b.nodes);
        assert_eq!(a.edges, b.edges);
        assert_eq!(a.external, b.external);
    }

    #[test]
    fn samples_are_capped_and_weight_counts_every_link() {
        let dir = tempdir().unwrap();
        for name in ["a", "b", "c", "d", "e"] {
            write(
                dir.path(),
                &format!("src/ui/{name}.ts"),
                "import { x } from \"../data/x\";\n",
            );
        }
        write(dir.path(), "src/data/x.ts", "export const x = 1;\n");
        let (state, _) = IndexState::build_from_root(dir.path());
        let g = build(
            &state,
            &Options {
                granularity: Granularity::Dir,
                ..Options::default()
            },
        );
        assert_eq!(g.edges.len(), 1);
        assert_eq!(g.edges[0].weight, 5);
        let sources: Vec<&str> = g.edges[0].sample.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(sources, ["src/ui/a.ts", "src/ui/b.ts", "src/ui/c.ts"]);
    }

    #[test]
    fn rejects_bad_arguments() {
        let bad = |pairs: Vec<(&str, VmValue)>| parse_options(&[build_dict(pairs)]).is_err();
        assert!(bad(vec![("granularity", str_value("package"))]));
        assert!(bad(vec![("depth", VmValue::Int(0))]));
        assert!(bad(vec![("min_weight", VmValue::Int(0))]));
        let ok = parse_options(&[build_dict([("depth", VmValue::Nil)])]).unwrap();
        assert_eq!(ok.depth, None);
        assert_eq!(ok.granularity, Granularity::Module);
    }
}
