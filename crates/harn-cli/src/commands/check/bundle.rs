use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use harn_modules::resolve_import_path;
use harn_parser::{Node, SNode};

use crate::package::CheckConfig;
use crate::parse_source_file;

use super::preflight::{
    dict_literal_field, host_render_path_arg, literal_string, parse_host_call_args,
    resolve_preflight_target, resolve_source_relative,
};

#[derive(Debug, Clone)]
struct BundleModuleRecord {
    path: String,
    role: &'static str,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
struct BundleImportEdge {
    from: String,
    to: String,
}

#[derive(Debug, Clone)]
struct BundleAssetRecord {
    declared_in: String,
    via: String,
    kind: &'static str,
    target: String,
    resolved: String,
    candidates: Vec<String>,
    exists: bool,
}

#[derive(Debug, Default)]
struct BundleManifestBuilder {
    modules: BTreeMap<String, BundleModuleRecord>,
    import_edges: BTreeSet<BundleImportEdge>,
    assets: BTreeMap<String, BundleAssetRecord>,
    required_host_capabilities: BTreeMap<String, BTreeSet<String>>,
    execution_dirs: BTreeSet<String>,
    worktree_repos: BTreeSet<String>,
}

impl BundleManifestBuilder {
    fn add_module(&mut self, path: &Path, role: &'static str) {
        let key = path.display().to_string();
        self.modules
            .entry(key.clone())
            .or_insert(BundleModuleRecord { path: key, role });
    }

    fn add_import_edge(&mut self, from: &Path, to: &Path) {
        self.import_edges.insert(BundleImportEdge {
            from: from.display().to_string(),
            to: to.display().to_string(),
        });
    }

    fn add_asset(
        &mut self,
        declared_in: &Path,
        via: &str,
        target: &str,
        candidates: &[PathBuf],
        kind: &'static str,
    ) {
        let resolved = candidates
            .iter()
            .find(|path| path.exists())
            .or_else(|| candidates.first())
            .cloned()
            .unwrap_or_else(|| PathBuf::from(target));
        let key = format!(
            "{}\u{0}{}\u{0}{}",
            declared_in.display(),
            via,
            resolved.display()
        );
        self.assets.entry(key).or_insert(BundleAssetRecord {
            declared_in: declared_in.display().to_string(),
            via: via.to_string(),
            kind,
            target: target.to_string(),
            resolved: resolved.display().to_string(),
            candidates: candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
            exists: candidates.iter().any(|path| path.exists()),
        });
    }

    fn add_host_capability(&mut self, capability: &str, operation: &str) {
        self.required_host_capabilities
            .entry(capability.to_string())
            .or_default()
            .insert(operation.to_string());
    }

    fn to_json(&self, targets: &[PathBuf], config: &CheckConfig) -> serde_json::Value {
        let modules = self.modules.values().collect::<Vec<_>>();
        let entry_modules = modules
            .iter()
            .filter(|module| module.role == "entry")
            .map(|module| module.path.clone())
            .collect::<Vec<_>>();
        let import_modules = modules
            .iter()
            .filter(|module| module.role == "import")
            .map(|module| module.path.clone())
            .collect::<Vec<_>>();
        let modules = modules
            .iter()
            .map(|module| {
                serde_json::json!({
                    "path": module.path,
                    "role": module.role,
                })
            })
            .collect::<Vec<_>>();
        let assets = self.assets.values().collect::<Vec<_>>();
        let prompt_assets = assets
            .iter()
            .filter(|asset| asset.kind == "prompt_asset")
            .map(|asset| asset.resolved.clone())
            .collect::<Vec<_>>();
        let template_assets = assets
            .iter()
            .filter(|asset| asset.kind == "template_asset")
            .map(|asset| asset.resolved.clone())
            .collect::<Vec<_>>();
        let assets = assets
            .iter()
            .map(|asset| {
                serde_json::json!({
                    "declared_in": asset.declared_in,
                    "via": asset.via,
                    "kind": asset.kind,
                    "target": asset.target,
                    "resolved": asset.resolved,
                    "candidates": asset.candidates,
                    "exists": asset.exists,
                })
            })
            .collect::<Vec<_>>();
        let module_dependencies = self
            .import_edges
            .iter()
            .map(|edge| {
                serde_json::json!({
                    "from": edge.from,
                    "to": edge.to,
                })
            })
            .collect::<Vec<_>>();
        let required_host_capabilities = self
            .required_host_capabilities
            .iter()
            .map(|(capability, ops)| (capability.clone(), ops.iter().cloned().collect::<Vec<_>>()))
            .collect::<BTreeMap<_, _>>();
        serde_json::json!({
            "version": 1,
            "targets": targets.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
            "bundle_root": config.bundle_root,
            "entry_modules": entry_modules,
            "import_modules": import_modules,
            "modules": modules,
            "module_dependencies": module_dependencies,
            "prompt_assets": prompt_assets,
            "template_assets": template_assets,
            "assets": assets,
            "required_host_capabilities": required_host_capabilities,
            "execution_dirs": self.execution_dirs.iter().cloned().collect::<Vec<_>>(),
            "worktree_repos": self.worktree_repos.iter().cloned().collect::<Vec<_>>(),
            "summary": {
                "entry_module_count": self.modules.values().filter(|module| module.role == "entry").count(),
                "import_module_count": self.modules.values().filter(|module| module.role == "import").count(),
                "module_dependency_count": self.import_edges.len(),
                "prompt_asset_count": self.assets.values().filter(|asset| asset.kind == "prompt_asset").count(),
                "template_asset_count": self.assets.values().filter(|asset| asset.kind == "template_asset").count(),
                "host_capability_count": self.required_host_capabilities.len(),
                "execution_dir_count": self.execution_dirs.len(),
                "worktree_repo_count": self.worktree_repos.len(),
            },
        })
    }
}

fn classify_bundle_asset(target: &str, via: &str) -> &'static str {
    // Package-root forms address prompt assets by stable name; treat
    // them as prompt assets even when the file extension is omitted
    // (e.g. `@partials/tool-examples`).
    if via == "render_prompt"
        || target.ends_with(".harn.prompt")
        || target.ends_with(".prompt")
        || target.starts_with('@')
    {
        "prompt_asset"
    } else {
        "template_asset"
    }
}

fn scan_program_bundle(
    file_path: &Path,
    program: &[SNode],
    config: &CheckConfig,
    visited: &mut HashSet<PathBuf>,
    manifest: &mut BundleManifestBuilder,
) {
    let canonical = file_path
        .canonicalize()
        .unwrap_or_else(|_| file_path.to_path_buf());
    if !visited.insert(canonical.clone()) {
        return;
    }
    manifest.add_module(&canonical, "import");
    for node in program {
        scan_node_bundle(node, &canonical, config, visited, manifest);
    }
}

fn scan_node_bundle(
    node: &SNode,
    file_path: &Path,
    config: &CheckConfig,
    visited: &mut HashSet<PathBuf>,
    manifest: &mut BundleManifestBuilder,
) {
    match &node.node {
        Node::ImportDecl { path, .. } | Node::SelectiveImport { path, .. } => {
            if path.starts_with("std/") {
                return;
            }
            if let Some(import_path) = resolve_import_path(file_path, path) {
                let import_str = import_path.to_string_lossy().into_owned();
                let (_, import_program) = parse_source_file(&import_str);
                manifest.add_module(&import_path, "import");
                manifest.add_import_edge(file_path, &import_path);
                scan_program_bundle(&import_path, &import_program, config, visited, manifest);
            }
        }
        Node::FunctionCall { name, args } if name == "render" || name == "render_prompt" => {
            if let Some(template_path) = args.first().and_then(literal_string) {
                let candidates = resolve_preflight_target(file_path, &template_path, config);
                manifest.add_asset(
                    file_path,
                    name,
                    &template_path,
                    &candidates,
                    classify_bundle_asset(&template_path, name),
                );
            }
            let children = args.iter().collect::<Vec<_>>();
            scan_children_bundle(&children, file_path, config, visited, manifest);
        }
        Node::FunctionCall { name, args } if name == "host_call" => {
            if let Some((cap, op, params_arg)) = parse_host_call_args(args) {
                manifest.add_host_capability(&cap, &op);
                if cap == "template" && op == "render" {
                    if let Some(template_path) = host_render_path_arg(params_arg) {
                        let candidates =
                            resolve_preflight_target(file_path, &template_path, config);
                        manifest.add_asset(
                            file_path,
                            "host_call(template.render)",
                            &template_path,
                            &candidates,
                            classify_bundle_asset(&template_path, "host_call(template.render)"),
                        );
                    }
                }
            }
            let children = args.iter().collect::<Vec<_>>();
            scan_children_bundle(&children, file_path, config, visited, manifest);
        }
        Node::FunctionCall { name, args } if name == "exec_at" || name == "shell_at" => {
            if let Some(dir) = args.first().and_then(literal_string) {
                manifest.execution_dirs.insert(
                    resolve_source_relative(file_path, &dir)
                        .display()
                        .to_string(),
                );
            }
            let children = args.iter().collect::<Vec<_>>();
            scan_children_bundle(&children, file_path, config, visited, manifest);
        }
        Node::FunctionCall { name, args } if name == "spawn_agent" => {
            if let Some(config_node) = args.first() {
                collect_spawn_agent_bundle(config_node, file_path, manifest);
            }
            let children = args.iter().collect::<Vec<_>>();
            scan_children_bundle(&children, file_path, config, visited, manifest);
        }
        _ => {
            let children = node_children_bundle(node);
            scan_children_bundle(&children, file_path, config, visited, manifest);
        }
    }
}

fn node_children_bundle(node: &SNode) -> Vec<&SNode> {
    match &node.node {
        Node::Pipeline { body, .. }
        | Node::OverrideDecl { body, .. }
        | Node::SpawnExpr { body }
        | Node::Block(body)
        | Node::Closure { body, .. }
        | Node::TryExpr { body }
        | Node::MutexBlock { body }
        | Node::DeferStmt { body } => body.iter().collect(),
        Node::DeadlineBlock { duration, body } => {
            let mut children = vec![duration.as_ref()];
            children.extend(body.iter());
            children
        }
        Node::FnDecl { body, params, .. } | Node::ToolDecl { body, params, .. } => {
            let mut children = body.iter().collect::<Vec<_>>();
            for param in params {
                if let Some(default_value) = param.default_value.as_deref() {
                    children.push(default_value);
                }
            }
            children
        }
        Node::SkillDecl { fields, .. } => fields.iter().map(|(_, v)| v).collect(),
        Node::IfElse {
            condition,
            then_body,
            else_body,
        } => {
            let mut children = vec![condition.as_ref()];
            children.extend(then_body.iter());
            if let Some(else_body) = else_body {
                children.extend(else_body.iter());
            }
            children
        }
        Node::ForIn { iterable, body, .. } => {
            let mut children = vec![iterable.as_ref()];
            children.extend(body.iter());
            children
        }
        Node::MatchExpr { value, arms } => {
            let mut children = vec![value.as_ref()];
            for arm in arms {
                children.push(&arm.pattern);
                children.extend(arm.body.iter());
            }
            children
        }
        Node::WhileLoop { condition, body }
        | Node::GuardStmt {
            condition,
            else_body: body,
        } => {
            let mut children = vec![condition.as_ref()];
            children.extend(body.iter());
            children
        }
        Node::Retry { count, body } => {
            let mut children = vec![count.as_ref()];
            children.extend(body.iter());
            children
        }
        Node::ReturnStmt { value } | Node::YieldExpr { value } => {
            value.iter().map(|value| value.as_ref()).collect()
        }
        Node::EmitExpr { value } => vec![value.as_ref()],
        Node::TryCatch {
            body,
            catch_body,
            finally_body,
            ..
        } => {
            let mut children = body.iter().collect::<Vec<_>>();
            children.extend(catch_body.iter());
            if let Some(finally_body) = finally_body {
                children.extend(finally_body.iter());
            }
            children
        }
        Node::RequireStmt { condition, message } => {
            let mut children = vec![condition.as_ref()];
            if let Some(message) = message.as_deref() {
                children.push(message);
            }
            children
        }
        Node::DictLiteral(fields) | Node::StructConstruct { fields, .. } => {
            let mut children = Vec::new();
            for field in fields {
                children.push(&field.key);
                children.push(&field.value);
            }
            children
        }
        Node::Parallel { expr, body, .. } => {
            let mut children = vec![expr.as_ref()];
            children.extend(body.iter());
            children
        }
        Node::SelectExpr {
            cases,
            timeout,
            default_body,
        } => {
            let mut children = Vec::new();
            for case in cases {
                children.push(case.channel.as_ref());
                children.extend(case.body.iter());
            }
            if let Some((duration, body)) = timeout {
                children.push(duration.as_ref());
                children.extend(body.iter());
            }
            if let Some(default_body) = default_body {
                children.extend(default_body.iter());
            }
            children
        }
        Node::FunctionCall { args, .. } => args.iter().collect(),
        Node::MethodCall { object, args, .. } | Node::OptionalMethodCall { object, args, .. } => {
            let mut children = vec![object.as_ref()];
            children.extend(args.iter());
            children
        }
        Node::PropertyAccess { object, .. }
        | Node::OptionalPropertyAccess { object, .. }
        | Node::UnaryOp {
            operand: object, ..
        }
        | Node::ThrowStmt { value: object }
        | Node::Spread(object)
        | Node::TryOperator { operand: object }
        | Node::TryStar { operand: object } => vec![object.as_ref()],
        Node::SubscriptAccess { object, index }
        | Node::OptionalSubscriptAccess { object, index } => {
            vec![object.as_ref(), index.as_ref()]
        }
        Node::SliceAccess { object, start, end } => {
            let mut children = vec![object.as_ref()];
            if let Some(start) = start.as_deref() {
                children.push(start);
            }
            if let Some(end) = end.as_deref() {
                children.push(end);
            }
            children
        }
        Node::BinaryOp { left, right, .. }
        | Node::Assignment {
            target: left,
            value: right,
            ..
        } => {
            vec![left.as_ref(), right.as_ref()]
        }
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => vec![condition.as_ref(), true_expr.as_ref(), false_expr.as_ref()],
        Node::EnumConstruct { args, .. } | Node::ListLiteral(args) => args.iter().collect(),
        Node::LetBinding { value, .. } | Node::VarBinding { value, .. } => vec![value.as_ref()],
        Node::RangeExpr { start, end, .. } => vec![start.as_ref(), end.as_ref()],
        Node::ImplBlock { methods, .. } => methods.iter().collect(),
        Node::ImportDecl { .. }
        | Node::SelectiveImport { .. }
        | Node::EnumDecl { .. }
        | Node::StructDecl { .. }
        | Node::InterfaceDecl { .. }
        | Node::TypeDecl { .. }
        | Node::InterpolatedString(_)
        | Node::StringLiteral(_)
        | Node::RawStringLiteral(_)
        | Node::IntLiteral(_)
        | Node::FloatLiteral(_)
        | Node::BoolLiteral(_)
        | Node::NilLiteral
        | Node::Identifier(_)
        | Node::DurationLiteral(_)
        | Node::BreakStmt
        | Node::ContinueStmt => Vec::new(),
        Node::AttributedDecl { inner, .. } => vec![inner.as_ref()],
        Node::OrPattern(alternatives) => alternatives.iter().collect(),
    }
}

fn scan_children_bundle(
    children: &[&SNode],
    file_path: &Path,
    config: &CheckConfig,
    visited: &mut HashSet<PathBuf>,
    manifest: &mut BundleManifestBuilder,
) {
    for child in children {
        scan_node_bundle(child, file_path, config, visited, manifest);
    }
}

fn collect_spawn_agent_bundle(
    config_node: &SNode,
    file_path: &Path,
    manifest: &mut BundleManifestBuilder,
) {
    let Some(execution) = dict_literal_field(config_node, "execution") else {
        return;
    };
    if let Some(cwd) = dict_literal_field(execution, "cwd").and_then(literal_string) {
        manifest.execution_dirs.insert(
            resolve_source_relative(file_path, &cwd)
                .display()
                .to_string(),
        );
    }
    let Some(worktree) = dict_literal_field(execution, "worktree") else {
        return;
    };
    if let Some(repo) = dict_literal_field(worktree, "repo").and_then(literal_string) {
        manifest.worktree_repos.insert(
            resolve_source_relative(file_path, &repo)
                .display()
                .to_string(),
        );
    }
}

pub(crate) fn build_bundle_manifest(
    targets: &[PathBuf],
    config: &CheckConfig,
) -> serde_json::Value {
    let mut visited = HashSet::new();
    let mut manifest = BundleManifestBuilder::default();
    for target in targets {
        let canonical = target
            .canonicalize()
            .unwrap_or_else(|_| target.to_path_buf());
        manifest.add_module(&canonical, "entry");
        let target_str = canonical.to_string_lossy().into_owned();
        let (source, program) = parse_source_file(&target_str);
        let _ = source;
        scan_program_bundle(&canonical, &program, config, &mut visited, &mut manifest);
    }
    manifest.to_json(targets, config)
}
