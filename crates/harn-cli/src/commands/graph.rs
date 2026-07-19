use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use harn_ir::{CallClassification, Capability, LiteralValue, NodeSemantics};
use harn_parser::{
    format_type, parse_stdlib_metadata, synthesize_example, Node, SNode, StdlibMetadata, TypeExpr,
    TypeParam, TypedParam, Variance, WhereClause,
};
use serde::Serialize;

use crate::cli::GraphArgs;
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;
use crate::json_envelope::{to_string_pretty, JsonEnvelope};

pub(crate) const GRAPH_SCHEMA_VERSION: u32 = 1;

/// Env var the embedded `cli/graph.harn` script reads to pick up the
/// pre-serialised module-graph view the Rust shim derived from the
/// on-disk module cache + IR analyser. Renaming requires updating both
/// the shim and the script in lockstep.
const GRAPH_VIEW_ENV: &str = "HARN_GRAPH_VIEW_JSON";

/// Serialises the dispatch path so concurrent in-process callers don't
/// race on the global env vars the shim sets. Matches the lock pattern
/// in `commands/routes.rs` / `commands/models/list.rs`.
static DISPATCH_GRAPH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GraphReport {
    pub modules: Vec<GraphModule>,
    pub graph: GraphEdges,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GraphModule {
    pub path: String,
    pub public_symbols: Vec<GraphSymbol>,
    pub imports: Vec<String>,
    pub requires_capabilities: Vec<String>,
    pub effects: Vec<String>,
    pub host_calls: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GraphSymbol {
    pub name: String,
    pub kind: String,
    pub signature: String,
    /// Declared `@effects`/`@errors` (+ optional `@api_stability`/
    /// `@example`) block parsed from the HarnDoc above the declaration.
    /// Surfaced for `harn graph --json` consumers (docs, IDE hover,
    /// agents); absent when the author has not annotated the function.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<StdlibMetadata>,
    /// Example synthesized from the type signature; present only when the
    /// doc block has no `@example` (consumers prefer `metadata.example`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derived_example: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GraphEdges {
    pub nodes: Vec<String>,
    pub edges: Vec<GraphEdge>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GraphEdge {
    pub from: String,
    pub to: String,
}

/// Run `harn graph`. Rust owns module-graph extraction; the embedded
/// `cli/graph.harn` script owns rendering. Extraction failures render the
/// error envelope / stderr line directly because no view exists to pass into
/// the script.
pub(crate) async fn run(args: GraphArgs) -> i32 {
    let report = match analyze_graph(&args.root, args.module.as_deref()) {
        Ok(report) => report,
        Err(error) => {
            if args.json {
                let envelope: JsonEnvelope<GraphReport> =
                    JsonEnvelope::err(GRAPH_SCHEMA_VERSION, "graph_error", error);
                println!("{}", to_string_pretty(&envelope));
            } else {
                eprintln!("error: {error}");
            }
            return 1;
        }
    };

    run_dispatch(&report, args.json).await
}

async fn run_dispatch(report: &GraphReport, json: bool) -> i32 {
    let view_json = match serde_json::to_string(report) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("internal error: failed to serialise graph view: {error}");
            return 1;
        }
    };
    let _guard = DISPATCH_GRAPH_LOCK.lock().await;
    let _view = ScopedEnvVar::set(GRAPH_VIEW_ENV, &view_json);
    let outcome = dispatch::run_embedded_script("graph", Vec::new(), json).await;
    if !outcome.stderr.is_empty() {
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    if !outcome.stdout.is_empty() {
        let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    }
    outcome.exit_code
}

fn analyze_graph(root: &Path, module_filter: Option<&str>) -> Result<GraphReport, String> {
    let root_dir = graph_root_dir(root);
    let targets = [root.to_string_lossy().to_string()];
    let target_refs: Vec<&str> = targets.iter().map(String::as_str).collect();
    let files = crate::commands::check::collect_harn_targets(&target_refs);
    if files.is_empty() {
        return Err(format!("no .harn files found under {}", root.display()));
    }

    let module_graph = crate::commands::check::build_module_graph(&files);
    let mut module_paths = module_graph.module_paths();
    if let Some(filter) = module_filter {
        module_paths.retain(|path| module_matches_filter(path, filter, &root_dir));
    }

    let display_paths: BTreeMap<PathBuf, String> = module_graph
        .module_paths()
        .into_iter()
        .map(|path| {
            let display = display_module_path(&path, &root_dir);
            (path, display)
        })
        .collect();

    let mut modules = Vec::new();
    let mut edges = Vec::new();
    let mut nodes = BTreeSet::new();
    for path in &module_paths {
        let display_path = display_paths
            .get(path)
            .cloned()
            .unwrap_or_else(|| display_module_path(path, &root_dir));
        nodes.insert(display_path.clone());
        let imports = module_graph.imports_for_module(path);
        for import in &imports {
            if let Some(resolved) = &import.resolved_path {
                let to = display_paths
                    .get(resolved)
                    .cloned()
                    .unwrap_or_else(|| display_module_path(resolved, &root_dir));
                nodes.insert(to.clone());
                edges.push(GraphEdge {
                    from: display_path.clone(),
                    to,
                });
            }
        }
        modules.push(describe_module(
            path,
            &display_path,
            &root_dir,
            &module_graph,
            &imports,
        )?);
    }

    modules.sort_by(|left, right| left.path.cmp(&right.path));
    edges.sort_by(|left, right| {
        left.from
            .cmp(&right.from)
            .then_with(|| left.to.cmp(&right.to))
    });
    edges.dedup_by(|left, right| left.from == right.from && left.to == right.to);

    Ok(GraphReport {
        modules,
        graph: GraphEdges {
            nodes: nodes.into_iter().collect(),
            edges,
        },
    })
}

fn describe_module(
    path: &Path,
    display_path: &str,
    root_dir: &Path,
    module_graph: &harn_modules::ModuleGraph,
    imports: &[harn_modules::ModuleImport],
) -> Result<GraphModule, String> {
    let source = harn_modules::read_module_source(path)
        .ok_or_else(|| format!("failed to read {}", path.display()))?;
    let program = harn_parser::parse_source(&source)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
    let exports: BTreeSet<String> = module_graph.exports_for_module(path).into_iter().collect();
    let public_symbols = public_symbols(&program, &exports, &source);
    let usage = module_usage(&program);

    Ok(GraphModule {
        path: display_path.to_string(),
        public_symbols,
        imports: imports
            .iter()
            .map(|import| {
                import
                    .resolved_path
                    .as_deref()
                    .map(|resolved| display_module_path(resolved, root_dir))
                    .unwrap_or_else(|| import.raw_path.clone())
            })
            .collect(),
        requires_capabilities: usage.capabilities.into_iter().collect(),
        effects: usage.effects.into_iter().collect(),
        host_calls: usage.host_calls.into_iter().collect(),
    })
}

#[derive(Debug, Default)]
struct ModuleUsage {
    capabilities: BTreeSet<String>,
    effects: BTreeSet<String>,
    host_calls: BTreeSet<String>,
}

fn module_usage(program: &[SNode]) -> ModuleUsage {
    let report = harn_ir::analyze_program(program);
    let mut usage = ModuleUsage::default();
    for handler in report.handlers {
        for node in handler.nodes {
            let NodeSemantics::Call(call) = node.semantics else {
                continue;
            };
            usage.host_calls.extend(host_call_surface(&call));
            usage.capabilities.extend(direct_capabilities(&call));
            usage.effects.extend(direct_effects(&call));
            if let CallClassification::Capabilities(effects) = call.classification {
                for effect in effects {
                    usage
                        .capabilities
                        .insert(capability_label(effect.capability));
                    usage
                        .effects
                        .insert(effect_label(effect.capability, &effect.operation));
                    usage.host_calls.insert(call.display_name.clone());
                }
            }
        }
    }
    usage
}

fn direct_capabilities(call: &harn_ir::CallSemantics) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    match call.name.as_str() {
        "read_file" | "read_file_bytes" | "read_file_result" | "read_lines" | "read_stdin" => {
            out.insert("workspace.read_text".to_string());
        }
        "list_dir" | "walk_dir" | "glob" | "find_text" => {
            out.insert("workspace.list".to_string());
        }
        "file_exists" | "path_status" | "stat" => {
            out.insert("workspace.exists".to_string());
        }
        "write_file" | "write_file_bytes" | "append_file" | "mkdir" | "mkdtemp" | "copy_file"
        | "move_file" => {
            out.insert("workspace.write_text".to_string());
        }
        "delete_file" => {
            out.insert("workspace.delete".to_string());
        }
        "apply_edit" => {
            out.insert("workspace.apply_edit".to_string());
        }
        "term_width" | "term_height" => out.extend(["terminal.dimensions".to_string()]),
        "read_password" => out.extend(["terminal.read_password".to_string()]),
        "http_get"
        | "http_post"
        | "http_put"
        | "http_patch"
        | "http_delete"
        | "http_request"
        | "http_download"
        | "http_session"
        | "http_session_request"
        | "http_stream_open"
        | "sse_connect"
        | "websocket_connect"
        | "websocket_server" => {
            out.insert("network.http".to_string());
        }
        "exec" | "exec_at" | "shell" | "shell_at" | "spawn_captured" => {
            out.insert("process.exec".to_string());
        }
        "llm_call"
        | "llm_call_safe"
        | "llm_stream_call"
        | "llm_call_structured"
        | "llm_call_structured_safe"
        | "llm_call_structured_result"
        | "llm_completion"
        | "agent_llm_turn"
        | "agent_turn"
        | "agent_loop" => {
            out.insert("llm.call".to_string());
        }
        "llm_catalog" | "llm_catalog_refresh" | "llm_provider_status" => {
            out.insert("llm.catalog".to_string());
        }
        "spawn_agent" | "send_input" | "resume_agent" | "wait_agent" | "close_agent"
        | "worker_trigger" => {
            out.insert("worker.dispatch".to_string());
        }
        "request_approval" => out.extend(["human.approval".to_string()]),
        "connector_call" | "mcp_call" | "host_tool_call" => {
            out.insert("connector.call".to_string());
        }
        "secret_get" => out.extend(["connector.secret_get".to_string()]),
        "host_call" => insert_host_call_capability(call, &mut out),
        _ if call.name.starts_with("mcp_") => {
            out.insert("connector.call".to_string());
        }
        _ => {}
    }
    if call.name == "find_text" {
        out.insert("workspace.read_text".to_string());
    }
    out
}

fn direct_effects(call: &harn_ir::CallSemantics) -> BTreeSet<String> {
    direct_capabilities(call)
        .into_iter()
        .map(|capability| match capability.as_str() {
            "workspace.read_text" | "workspace.list" | "workspace.exists" => "fs.read".to_string(),
            "workspace.write_text" | "workspace.delete" | "workspace.apply_edit" => {
                "fs.write".to_string()
            }
            "network.http" => "net.http".to_string(),
            "process.exec" => "process.exec".to_string(),
            "llm.call" => "llm.call".to_string(),
            "llm.catalog" => "llm.catalog".to_string(),
            "worker.dispatch" => "worker.dispatch".to_string(),
            "human.approval" => "human.approval".to_string(),
            "template.render" => "template.render".to_string(),
            "terminal.dimensions" | "terminal.read_password" => "terminal.read".to_string(),
            other if other.starts_with("connector.") => "connector.call".to_string(),
            other => other.to_string(),
        })
        .collect()
}

fn host_call_surface(call: &harn_ir::CallSemantics) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if !direct_capabilities(call).is_empty()
        || matches!(call.display_name.as_str(), "harness.crypto.sha256")
    {
        out.insert(call.display_name.clone());
    }
    if call.name == "host_call" {
        if let Some(operation) = call.literal_args.first().and_then(literal_as_str) {
            out.insert(operation.to_string());
        }
    }
    out
}

fn insert_host_call_capability(call: &harn_ir::CallSemantics, out: &mut BTreeSet<String>) {
    let Some(operation) = call.literal_args.first().and_then(literal_as_str) else {
        out.insert("connector.call".to_string());
        return;
    };
    if operation == "template.render" {
        out.insert("template.render".to_string());
    } else if operation.starts_with("process.") {
        out.insert("process.exec".to_string());
    } else if operation.starts_with("workspace.") || operation.starts_with("connector.") {
        out.insert(operation.to_string());
    } else {
        out.insert("connector.call".to_string());
    }
}

fn literal_as_str(value: &LiteralValue) -> Option<&str> {
    match value {
        LiteralValue::String(value) | LiteralValue::Identifier(value) => Some(value.as_str()),
        _ => None,
    }
}

fn capability_label(capability: Capability) -> String {
    match capability {
        Capability::WorkspaceMutation => "workspace.write_text",
        Capability::CommandExecution => "process.exec",
        Capability::NetworkAccess => "network.http",
        Capability::ConnectorAccess => "connector.call",
        Capability::ModelCall => "llm.call",
        Capability::WorkerDispatch => "worker.dispatch",
        Capability::HumanApproval => "human.approval",
        Capability::AutonomyPolicy => "autonomy.policy",
    }
    .to_string()
}

fn effect_label(capability: Capability, operation: &str) -> String {
    match capability {
        Capability::WorkspaceMutation => "fs.write".to_string(),
        Capability::CommandExecution => "process.exec".to_string(),
        Capability::NetworkAccess => "net.http".to_string(),
        Capability::ConnectorAccess => {
            if operation == "template.render" {
                "template.render".to_string()
            } else {
                "connector.call".to_string()
            }
        }
        Capability::ModelCall => "llm.call".to_string(),
        Capability::WorkerDispatch => "worker.dispatch".to_string(),
        Capability::HumanApproval => "human.approval".to_string(),
        Capability::AutonomyPolicy => "autonomy.policy".to_string(),
    }
}

fn public_symbols(program: &[SNode], exports: &BTreeSet<String>, source: &str) -> Vec<GraphSymbol> {
    let mut symbols = Vec::new();
    for node in program {
        collect_public_symbol(node, exports, source, &mut symbols);
    }
    symbols.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.kind.cmp(&right.kind))
    });
    symbols
}

/// Synthesize a signature-derived example for a callable, unless the doc
/// block already carries a hand-written `@example`.
fn derived_example_for(
    source: &str,
    node: &SNode,
    name: &str,
    params: &[TypedParam],
    return_type: Option<&TypeExpr>,
) -> Option<String> {
    parse_stdlib_metadata(source, &node.span)
        .and_then(|meta| meta.example)
        .is_none()
        .then(|| synthesize_example(name, params, return_type))
}

fn collect_public_symbol(
    node: &SNode,
    exports: &BTreeSet<String>,
    source: &str,
    out: &mut Vec<GraphSymbol>,
) {
    match &node.node {
        Node::AttributedDecl { inner, .. } => collect_public_symbol(inner, exports, source, out),
        Node::FnDecl {
            name,
            type_params,
            params,
            return_type,
            where_clauses,
            is_stream,
            ..
        } if exports.contains(name) => out.push(GraphSymbol {
            name: name.clone(),
            kind: "fn".to_string(),
            signature: fn_signature(
                if *is_stream { "gen fn" } else { "fn" },
                name,
                type_params,
                params,
                return_type.as_ref(),
                where_clauses,
            ),
            metadata: parse_stdlib_metadata(source, &node.span).filter(|meta| !meta.is_empty()),
            derived_example: derived_example_for(source, node, name, params, return_type.as_ref()),
        }),
        Node::ToolDecl {
            name,
            params,
            return_type,
            ..
        } if exports.contains(name) => out.push(GraphSymbol {
            name: name.clone(),
            kind: "tool".to_string(),
            signature: callable_signature("tool", name, &[], params, return_type.as_ref(), &[]),
            metadata: parse_stdlib_metadata(source, &node.span).filter(|meta| !meta.is_empty()),
            derived_example: derived_example_for(source, node, name, params, return_type.as_ref()),
        }),
        Node::Pipeline {
            name,
            params,
            return_type,
            ..
        } if exports.contains(name) => out.push(GraphSymbol {
            name: name.clone(),
            kind: "pipeline".to_string(),
            signature: callable_signature("pipeline", name, &[], params, return_type.as_ref(), &[]),
            metadata: None,
            derived_example: None,
        }),
        Node::StructDecl {
            name, type_params, ..
        } if exports.contains(name) => out.push(GraphSymbol {
            name: name.clone(),
            kind: "struct".to_string(),
            signature: format!("struct {}{}", name, format_type_params(type_params)),
            metadata: None,
            derived_example: None,
        }),
        Node::EnumDecl {
            name, type_params, ..
        } if exports.contains(name) => out.push(GraphSymbol {
            name: name.clone(),
            kind: "enum".to_string(),
            signature: format!("enum {}{}", name, format_type_params(type_params)),
            metadata: None,
            derived_example: None,
        }),
        Node::InterfaceDecl {
            name, type_params, ..
        } if exports.contains(name) => out.push(GraphSymbol {
            name: name.clone(),
            kind: "interface".to_string(),
            signature: format!("interface {}{}", name, format_type_params(type_params)),
            metadata: None,
            derived_example: None,
        }),
        Node::TypeDecl {
            name,
            type_params,
            type_expr,
            ..
        } if exports.contains(name) => out.push(GraphSymbol {
            name: name.clone(),
            kind: "type".to_string(),
            signature: format!(
                "type {}{} = {}",
                name,
                format_type_params(type_params),
                format_type(type_expr)
            ),
            metadata: None,
            derived_example: None,
        }),
        Node::SkillDecl { name, .. } if exports.contains(name) => out.push(GraphSymbol {
            name: name.clone(),
            kind: "skill".to_string(),
            signature: format!("skill {name}"),
            metadata: None,
            derived_example: None,
        }),
        _ => {}
    }
}

fn fn_signature(
    keyword: &str,
    name: &str,
    type_params: &[TypeParam],
    params: &[TypedParam],
    return_type: Option<&harn_parser::TypeExpr>,
    where_clauses: &[WhereClause],
) -> String {
    callable_signature(
        keyword,
        name,
        type_params,
        params,
        return_type,
        where_clauses,
    )
}

fn callable_signature(
    keyword: &str,
    name: &str,
    type_params: &[TypeParam],
    params: &[TypedParam],
    return_type: Option<&harn_parser::TypeExpr>,
    where_clauses: &[WhereClause],
) -> String {
    let params = params
        .iter()
        .map(format_param)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{} {}{}({}){}{}",
        keyword,
        name,
        format_type_params(type_params),
        params,
        return_type
            .map(|ty| format!(" -> {}", format_type(ty)))
            .unwrap_or_default(),
        format_where_clauses(where_clauses)
    )
}

fn format_param(param: &TypedParam) -> String {
    let rest = if param.rest { "..." } else { "" };
    match &param.type_expr {
        Some(ty) => format!("{rest}{}: {}", param.name, format_type(ty)),
        None => format!("{rest}{}", param.name),
    }
}

fn format_type_params(params: &[TypeParam]) -> String {
    if params.is_empty() {
        return String::new();
    }
    let params = params
        .iter()
        .map(|param| match param.variance {
            Variance::Invariant => param.name.clone(),
            Variance::Covariant => format!("out {}", param.name),
            Variance::Contravariant => format!("in {}", param.name),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("<{params}>")
}

fn format_where_clauses(clauses: &[WhereClause]) -> String {
    if clauses.is_empty() {
        return String::new();
    }
    let clauses = clauses
        .iter()
        .map(|clause| format!("{}: {}", clause.type_name, format_type(&clause.bound)))
        .collect::<Vec<_>>()
        .join(", ");
    format!(" where {clauses}")
}

fn graph_root_dir(root: &Path) -> PathBuf {
    let base = if root.is_dir() {
        root
    } else {
        root.parent().unwrap_or(Path::new("."))
    };
    fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf())
}

fn display_module_path(path: &Path, root_dir: &Path) -> String {
    let raw = path.to_string_lossy();
    if raw.starts_with("<std>/") {
        return raw.replacen("<std>/", "std/", 1);
    }
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical
        .strip_prefix(root_dir)
        .unwrap_or(&canonical)
        .to_string_lossy()
        .trim_start_matches('/')
        .to_string()
}

fn module_matches_filter(path: &Path, filter: &str, root_dir: &Path) -> bool {
    let display = display_module_path(path, root_dir);
    display == filter
        || display.strip_suffix(".harn") == Some(filter)
        || path.file_stem().and_then(|stem| stem.to_str()) == Some(filter)
        || path.file_name().and_then(|name| name.to_str()) == Some(filter)
}
