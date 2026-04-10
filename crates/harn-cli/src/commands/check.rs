use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process;

use harn_fmt::{format_source_opts, FmtOptions};
use harn_lint::LintSeverity;
use harn_parser::{DiagnosticSeverity, Node, SNode, TypeChecker};

use crate::package::CheckConfig;
use crate::parse_source_file;

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct CommandOutcome {
    pub has_error: bool,
    pub has_warning: bool,
}

impl CommandOutcome {
    pub(crate) fn should_fail(self, strict: bool) -> bool {
        self.has_error || (strict && self.has_warning)
    }
}

fn print_lint_diagnostics(path: &str, diagnostics: &[harn_lint::LintDiagnostic]) -> bool {
    let mut has_error = false;
    for diag in diagnostics {
        let severity = match diag.severity {
            LintSeverity::Warning => "warning",
            LintSeverity::Error => {
                has_error = true;
                "error"
            }
        };
        println!(
            "{path}:{}:{}: {severity}[{}]: {}",
            diag.span.line, diag.span.column, diag.rule, diag.message
        );
        if let Some(ref suggestion) = diag.suggestion {
            println!("  suggestion: {suggestion}");
        }
    }
    has_error
}

pub(crate) fn check_file_inner(
    path: &Path,
    config: &CheckConfig,
    externally_imported_names: &std::collections::HashSet<String>,
) -> CommandOutcome {
    let path_str = path.to_string_lossy().to_string();
    let (source, program) = parse_source_file(&path_str);

    let mut has_error = false;
    let mut has_warning = false;
    let mut diagnostic_count = 0;

    // Type checking
    let type_diagnostics = TypeChecker::new().check(&program);
    for diag in &type_diagnostics {
        let severity = match diag.severity {
            DiagnosticSeverity::Error => {
                has_error = true;
                "error"
            }
            DiagnosticSeverity::Warning => {
                has_warning = true;
                "warning"
            }
        };
        diagnostic_count += 1;
        if let Some(span) = &diag.span {
            let rendered = harn_parser::diagnostic::render_diagnostic(
                &source,
                &path_str,
                span,
                severity,
                &diag.message,
                None,
                diag.help.as_deref(),
            );
            eprint!("{rendered}");
        } else {
            eprintln!("{severity}: {}", diag.message);
        }
    }

    // Linting
    let lint_diagnostics = harn_lint::lint_with_cross_file_imports(
        &program,
        &config.disable_rules,
        Some(&source),
        externally_imported_names,
    );
    diagnostic_count += lint_diagnostics.len();
    if lint_diagnostics
        .iter()
        .any(|d| d.severity == LintSeverity::Warning)
    {
        has_warning = true;
    }
    if print_lint_diagnostics(&path_str, &lint_diagnostics) {
        has_error = true;
    }

    let preflight_diagnostics = collect_preflight_diagnostics(path, &source, &program, config);
    for diag in &preflight_diagnostics {
        has_error = true;
        diagnostic_count += 1;
        let rendered = harn_parser::diagnostic::render_diagnostic(
            &diag.source,
            &diag.path,
            &diag.span,
            "error",
            &diag.message,
            Some("preflight failure"),
            diag.help.as_deref(),
        );
        eprint!("{rendered}");
    }

    if diagnostic_count == 0 {
        println!("{path_str}: ok");
    }

    CommandOutcome {
        has_error,
        has_warning,
    }
}

pub(crate) fn lint_file_inner(
    path: &Path,
    config: &CheckConfig,
    externally_imported_names: &std::collections::HashSet<String>,
) -> CommandOutcome {
    let path_str = path.to_string_lossy().to_string();
    let (source, program) = parse_source_file(&path_str);

    let diagnostics = harn_lint::lint_with_cross_file_imports(
        &program,
        &config.disable_rules,
        Some(&source),
        externally_imported_names,
    );

    if diagnostics.is_empty() {
        println!("{path_str}: no issues found");
        return CommandOutcome::default();
    }

    let has_warning = diagnostics
        .iter()
        .any(|d| d.severity == LintSeverity::Warning);
    let has_error = print_lint_diagnostics(&path_str, &diagnostics);

    CommandOutcome {
        has_error,
        has_warning,
    }
}

/// Apply autofix edits from lint and type-check diagnostics and write back to disk.
/// Returns the number of fixes applied.
pub(crate) fn lint_fix_file(
    path: &Path,
    config: &CheckConfig,
    externally_imported_names: &HashSet<String>,
) -> usize {
    let path_str = path.to_string_lossy().to_string();
    let (source, program) = parse_source_file(&path_str);

    // Collect lint fixes
    let lint_diags = harn_lint::lint_with_cross_file_imports(
        &program,
        &config.disable_rules,
        Some(&source),
        externally_imported_names,
    );

    // Collect type-check fixes
    let type_diags = TypeChecker::new().check_with_source(&program, &source);

    // Merge all fixable edits
    let mut edits: Vec<&harn_lexer::FixEdit> = lint_diags
        .iter()
        .filter_map(|d| d.fix.as_ref())
        .chain(type_diags.iter().filter_map(|d| d.fix.as_ref()))
        .flatten()
        .collect();

    if edits.is_empty() {
        return 0;
    }

    // Sort by span.start descending so we can apply in reverse order
    edits.sort_by(|a, b| b.span.start.cmp(&a.span.start));

    // Filter overlapping edits (keep the first = highest offset)
    let mut accepted: Vec<&harn_lexer::FixEdit> = Vec::new();
    for edit in &edits {
        let overlaps = accepted
            .iter()
            .any(|prev| edit.span.start < prev.span.end && edit.span.end > prev.span.start);
        if !overlaps {
            accepted.push(edit);
        }
    }

    // Apply edits in reverse order (already sorted descending)
    let mut result = source.clone();
    for edit in &accepted {
        let before = &result[..edit.span.start];
        let after = &result[edit.span.end..];
        result = format!("{before}{}{after}", edit.replacement);
    }

    let applied = accepted.len();
    std::fs::write(path, &result).unwrap_or_else(|e| {
        eprintln!("Failed to write {path_str}: {e}");
        process::exit(1);
    });

    println!("{path_str}: applied {applied} fix(es)");

    // Re-lint to report remaining issues
    let (source2, program2) = parse_source_file(&path_str);
    let remaining = harn_lint::lint_with_cross_file_imports(
        &program2,
        &config.disable_rules,
        Some(&source2),
        externally_imported_names,
    );
    if !remaining.is_empty() {
        print_lint_diagnostics(&path_str, &remaining);
    }

    applied
}

/// Format one or more files or directories. Accepts multiple targets.
pub(crate) fn fmt_targets(targets: &[&str], check_mode: bool, opts: &FmtOptions) {
    let mut files = Vec::new();
    for target in targets {
        let path = std::path::Path::new(target);
        if path.is_dir() {
            super::collect_harn_files(path, &mut files);
        } else {
            files.push(path.to_path_buf());
        }
    }
    if files.is_empty() {
        eprintln!("No .harn files found");
        process::exit(1);
    }
    let mut has_error = false;
    for file in &files {
        let path_str = file.to_string_lossy();
        if !fmt_file_inner(&path_str, check_mode, opts) {
            has_error = true;
        }
    }
    if has_error {
        process::exit(1);
    }
}

pub(crate) fn collect_harn_targets(targets: &[&str]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for target in targets {
        let path = Path::new(target);
        if path.is_dir() {
            super::collect_harn_files(path, &mut files);
        } else {
            files.push(path.to_path_buf());
        }
    }
    files.sort();
    files.dedup();
    files
}

/// Pre-scan all files and collect every function name that appears in a
/// selective import (`import { foo } from "..."`).  The union of these names
/// is passed to the linter so that library functions consumed by other files
/// are not falsely flagged as unused.
pub(crate) fn collect_cross_file_imports(files: &[PathBuf]) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    for file in files {
        let path_str = file.to_string_lossy().to_string();
        let (_, program) = parse_source_file(&path_str);
        names.extend(harn_lint::collect_selective_import_names(&program));
    }
    names
}

/// Format a single file. Returns true on success, false on error.
fn fmt_file_inner(path: &str, check_mode: bool, opts: &FmtOptions) -> bool {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error reading {path}: {e}");
            return false;
        }
    };

    let formatted = match format_source_opts(&source, opts) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{path}: {e}");
            return false;
        }
    };

    if check_mode {
        if source != formatted {
            eprintln!("{path}: would be reformatted");
            return false;
        }
    } else if source != formatted {
        match std::fs::write(path, &formatted) {
            Ok(()) => println!("formatted {path}"),
            Err(e) => {
                eprintln!("Error writing {path}: {e}");
                return false;
            }
        }
    }
    true
}

struct PreflightDiagnostic {
    path: String,
    source: String,
    span: harn_lexer::Span,
    message: String,
    help: Option<String>,
}

fn collect_preflight_diagnostics(
    path: &Path,
    source: &str,
    program: &[SNode],
    config: &CheckConfig,
) -> Vec<PreflightDiagnostic> {
    let mut diagnostics = Vec::new();
    let mut visited = HashSet::new();
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut host_capabilities = load_host_capabilities(config);
    let mut mocked_caps_visited = HashSet::new();
    collect_mock_host_capabilities(
        &canonical,
        source,
        program,
        &mut mocked_caps_visited,
        &mut host_capabilities,
    );
    scan_program_preflight(
        &canonical,
        source,
        program,
        config,
        &host_capabilities,
        &mut visited,
        &mut diagnostics,
    );

    // Import collision detection: collect all names exported by each import
    // and flag duplicates across different modules.
    scan_import_collisions(&canonical, source, program, &mut diagnostics);

    diagnostics
}

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
    if via == "render_prompt" || target.ends_with(".harn.prompt") || target.ends_with(".prompt") {
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
        Node::ImportDecl { path } | Node::SelectiveImport { path, .. } => {
            if path.starts_with("std/") {
                return;
            }
            if let Some(import_path) = resolve_import_path(file_path, path) {
                let import_str = import_path.to_string_lossy().to_string();
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
        | Node::TryOperator { operand: object } => vec![object.as_ref()],
        Node::SubscriptAccess { object, index } => vec![object.as_ref(), index.as_ref()],
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
        let target_str = canonical.to_string_lossy().to_string();
        let (source, program) = parse_source_file(&target_str);
        let _ = source;
        scan_program_bundle(&canonical, &program, config, &mut visited, &mut manifest);
    }
    manifest.to_json(targets, config)
}

fn collect_mock_host_capabilities(
    file_path: &Path,
    source: &str,
    program: &[SNode],
    visited: &mut HashSet<PathBuf>,
    capabilities: &mut HashMap<String, HashSet<String>>,
) {
    let canonical = file_path
        .canonicalize()
        .unwrap_or_else(|_| file_path.to_path_buf());
    if !visited.insert(canonical.clone()) {
        return;
    }
    for node in program {
        collect_mock_host_capabilities_from_node(node, &canonical, source, visited, capabilities);
    }
}

fn collect_mock_host_capabilities_from_node(
    node: &SNode,
    file_path: &Path,
    source: &str,
    visited: &mut HashSet<PathBuf>,
    capabilities: &mut HashMap<String, HashSet<String>>,
) {
    match &node.node {
        Node::Pipeline { body, .. }
        | Node::OverrideDecl { body, .. }
        | Node::FnDecl { body, .. }
        | Node::ToolDecl { body, .. }
        | Node::SpawnExpr { body }
        | Node::TryExpr { body }
        | Node::MutexBlock { body }
        | Node::DeferStmt { body }
        | Node::Block(body)
        | Node::Closure { body, .. } => {
            for child in body {
                collect_mock_host_capabilities_from_node(
                    child,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::ImportDecl { path } | Node::SelectiveImport { path, .. } => {
            if path.starts_with("std/") {
                return;
            }
            let Some(import_path) = resolve_import_path(file_path, path) else {
                return;
            };
            let import_str = import_path.to_string_lossy().to_string();
            let (import_source, import_program) = parse_source_file(&import_str);
            collect_mock_host_capabilities(
                &import_path,
                &import_source,
                &import_program,
                visited,
                capabilities,
            );
        }
        Node::IfElse {
            condition,
            then_body,
            else_body,
        } => {
            collect_mock_host_capabilities_from_node(
                condition,
                file_path,
                source,
                visited,
                capabilities,
            );
            for child in then_body {
                collect_mock_host_capabilities_from_node(
                    child,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
            if let Some(else_body) = else_body {
                for child in else_body {
                    collect_mock_host_capabilities_from_node(
                        child,
                        file_path,
                        source,
                        visited,
                        capabilities,
                    );
                }
            }
        }
        Node::ForIn { iterable, body, .. }
        | Node::WhileLoop {
            condition: iterable,
            body,
        } => {
            collect_mock_host_capabilities_from_node(
                iterable,
                file_path,
                source,
                visited,
                capabilities,
            );
            for child in body {
                collect_mock_host_capabilities_from_node(
                    child,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::Retry { count, body } => {
            collect_mock_host_capabilities_from_node(
                count,
                file_path,
                source,
                visited,
                capabilities,
            );
            for child in body {
                collect_mock_host_capabilities_from_node(
                    child,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::ReturnStmt { value } | Node::YieldExpr { value } => {
            if let Some(value) = value {
                collect_mock_host_capabilities_from_node(
                    value,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::RequireStmt { condition, message } => {
            collect_mock_host_capabilities_from_node(
                condition,
                file_path,
                source,
                visited,
                capabilities,
            );
            if let Some(message) = message {
                collect_mock_host_capabilities_from_node(
                    message,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::TryCatch {
            body,
            catch_body,
            finally_body,
            ..
        } => {
            for child in body {
                collect_mock_host_capabilities_from_node(
                    child,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
            for child in catch_body {
                collect_mock_host_capabilities_from_node(
                    child,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
            if let Some(finally_body) = finally_body {
                for child in finally_body {
                    collect_mock_host_capabilities_from_node(
                        child,
                        file_path,
                        source,
                        visited,
                        capabilities,
                    );
                }
            }
        }
        Node::GuardStmt {
            condition,
            else_body,
        } => {
            collect_mock_host_capabilities_from_node(
                condition,
                file_path,
                source,
                visited,
                capabilities,
            );
            for child in else_body {
                collect_mock_host_capabilities_from_node(
                    child,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::DictLiteral(fields) => {
            for field in fields {
                collect_mock_host_capabilities_from_node(
                    &field.value,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::DeadlineBlock { duration, body } => {
            collect_mock_host_capabilities_from_node(
                duration,
                file_path,
                source,
                visited,
                capabilities,
            );
            for child in body {
                collect_mock_host_capabilities_from_node(
                    child,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::Parallel { expr, body, .. } => {
            collect_mock_host_capabilities_from_node(
                expr,
                file_path,
                source,
                visited,
                capabilities,
            );
            for child in body {
                collect_mock_host_capabilities_from_node(
                    child,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::SelectExpr {
            cases,
            timeout,
            default_body,
        } => {
            for case in cases {
                collect_mock_host_capabilities_from_node(
                    &case.channel,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
                for child in &case.body {
                    collect_mock_host_capabilities_from_node(
                        child,
                        file_path,
                        source,
                        visited,
                        capabilities,
                    );
                }
            }
            if let Some((timeout_expr, body)) = timeout {
                collect_mock_host_capabilities_from_node(
                    timeout_expr,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
                for child in body {
                    collect_mock_host_capabilities_from_node(
                        child,
                        file_path,
                        source,
                        visited,
                        capabilities,
                    );
                }
            }
            if let Some(body) = default_body {
                for child in body {
                    collect_mock_host_capabilities_from_node(
                        child,
                        file_path,
                        source,
                        visited,
                        capabilities,
                    );
                }
            }
        }
        Node::FunctionCall { name, args } => {
            if name == "host_mock" {
                if let (Some(Node::StringLiteral(cap)), Some(Node::StringLiteral(op))) = (
                    args.first().map(|arg| &arg.node),
                    args.get(1).map(|arg| &arg.node),
                ) {
                    capabilities
                        .entry(cap.clone())
                        .or_default()
                        .insert(op.clone());
                }
            }
            for arg in args {
                collect_mock_host_capabilities_from_node(
                    arg,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::MethodCall { object, args, .. } | Node::OptionalMethodCall { object, args, .. } => {
            collect_mock_host_capabilities_from_node(
                object,
                file_path,
                source,
                visited,
                capabilities,
            );
            for arg in args {
                collect_mock_host_capabilities_from_node(
                    arg,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::PropertyAccess { object, .. }
        | Node::OptionalPropertyAccess { object, .. }
        | Node::UnaryOp {
            operand: object, ..
        }
        | Node::Spread(object)
        | Node::TryOperator { operand: object } => {
            collect_mock_host_capabilities_from_node(
                object,
                file_path,
                source,
                visited,
                capabilities,
            );
        }
        Node::SubscriptAccess { object, index } => {
            collect_mock_host_capabilities_from_node(
                object,
                file_path,
                source,
                visited,
                capabilities,
            );
            collect_mock_host_capabilities_from_node(
                index,
                file_path,
                source,
                visited,
                capabilities,
            );
        }
        Node::SliceAccess { object, start, end } => {
            collect_mock_host_capabilities_from_node(
                object,
                file_path,
                source,
                visited,
                capabilities,
            );
            if let Some(start) = start {
                collect_mock_host_capabilities_from_node(
                    start,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
            if let Some(end) = end {
                collect_mock_host_capabilities_from_node(
                    end,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::BinaryOp { left, right, .. }
        | Node::Assignment {
            target: left,
            value: right,
            ..
        } => {
            collect_mock_host_capabilities_from_node(
                left,
                file_path,
                source,
                visited,
                capabilities,
            );
            collect_mock_host_capabilities_from_node(
                right,
                file_path,
                source,
                visited,
                capabilities,
            );
        }
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => {
            collect_mock_host_capabilities_from_node(
                condition,
                file_path,
                source,
                visited,
                capabilities,
            );
            collect_mock_host_capabilities_from_node(
                true_expr,
                file_path,
                source,
                visited,
                capabilities,
            );
            collect_mock_host_capabilities_from_node(
                false_expr,
                file_path,
                source,
                visited,
                capabilities,
            );
        }
        Node::ThrowStmt { value } => {
            collect_mock_host_capabilities_from_node(
                value,
                file_path,
                source,
                visited,
                capabilities,
            );
        }
        Node::EnumConstruct { args, .. } | Node::ListLiteral(args) => {
            for arg in args {
                collect_mock_host_capabilities_from_node(
                    arg,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::StructConstruct { fields, .. } => {
            for field in fields {
                collect_mock_host_capabilities_from_node(
                    &field.value,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::LetBinding { value, .. } | Node::VarBinding { value, .. } => {
            collect_mock_host_capabilities_from_node(
                value,
                file_path,
                source,
                visited,
                capabilities,
            );
        }
        Node::MatchExpr { value, arms } => {
            collect_mock_host_capabilities_from_node(
                value,
                file_path,
                source,
                visited,
                capabilities,
            );
            for arm in arms {
                collect_mock_host_capabilities_from_node(
                    &arm.pattern,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
                for child in &arm.body {
                    collect_mock_host_capabilities_from_node(
                        child,
                        file_path,
                        source,
                        visited,
                        capabilities,
                    );
                }
            }
        }
        Node::ImplBlock { methods, .. } => {
            for method in methods {
                collect_mock_host_capabilities_from_node(
                    method,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::InterpolatedString(_)
        | Node::StringLiteral(_)
        | Node::RawStringLiteral(_)
        | Node::IntLiteral(_)
        | Node::FloatLiteral(_)
        | Node::BoolLiteral(_)
        | Node::NilLiteral
        | Node::Identifier(_)
        | Node::DurationLiteral(_)
        | Node::RangeExpr { .. }
        | Node::TypeDecl { .. }
        | Node::EnumDecl { .. }
        | Node::StructDecl { .. }
        | Node::InterfaceDecl { .. }
        | Node::BreakStmt
        | Node::ContinueStmt => {
            let _ = source;
        }
    }
}

/// Tracks the origin of an imported name for collision detection.
struct ImportedName {
    module_path: String,
}

/// Collect all function names that would be imported by each import statement
/// in the program, and flag collisions.
fn scan_import_collisions(
    file_path: &Path,
    source: &str,
    program: &[SNode],
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    let mut imported_names: std::collections::HashMap<String, ImportedName> =
        std::collections::HashMap::new();

    for node in program {
        match &node.node {
            Node::ImportDecl { path } => {
                if path.starts_with("std/") {
                    continue;
                }
                let Some(import_path) = resolve_import_path(file_path, path) else {
                    continue; // already diagnosed as unresolved
                };
                let import_str = import_path.to_string_lossy().to_string();
                let Ok(import_source) = std::fs::read_to_string(&import_path) else {
                    continue;
                };
                let names = collect_exported_names(&import_source);
                for name in names {
                    if let Some(existing) = imported_names.get(&name) {
                        if existing.module_path != import_str {
                            diagnostics.push(PreflightDiagnostic {
                                path: file_path.display().to_string(),
                                source: source.to_string(),
                                span: node.span,
                                message: format!(
                                    "preflight: import collision — '{name}' is exported by both '{}' and '{path}'",
                                    existing.module_path
                                ),
                                help: Some(format!(
                                    "use selective imports to disambiguate: import {{ {name} }} from \"...\""
                                )),
                            });
                        }
                    } else {
                        imported_names.insert(
                            name,
                            ImportedName {
                                module_path: import_str.clone(),
                            },
                        );
                    }
                }
            }
            Node::SelectiveImport { names, path } => {
                if path.starts_with("std/") {
                    continue;
                }
                let module_path = resolve_import_path(file_path, path)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.clone());
                for name in names {
                    if let Some(existing) = imported_names.get(name) {
                        if existing.module_path != module_path {
                            diagnostics.push(PreflightDiagnostic {
                                path: file_path.display().to_string(),
                                source: source.to_string(),
                                span: node.span,
                                message: format!(
                                    "preflight: import collision — '{name}' is exported by both '{}' and '{path}'",
                                    existing.module_path
                                ),
                                help: Some(
                                    "rename one of the imported modules or avoid importing conflicting names"
                                        .to_string(),
                                ),
                            });
                        }
                    } else {
                        imported_names.insert(
                            name.clone(),
                            ImportedName {
                                module_path: module_path.clone(),
                            },
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

/// Parse a module source and extract the names it would export via wildcard import.
fn collect_exported_names(source: &str) -> Vec<String> {
    let mut lexer = harn_lexer::Lexer::new(source);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let mut parser = harn_parser::Parser::new(tokens);
    let program = match parser.parse() {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    let has_pub = program
        .iter()
        .any(|n| matches!(&n.node, Node::FnDecl { is_pub: true, .. }));
    program
        .iter()
        .filter_map(|n| match &n.node {
            Node::FnDecl { name, is_pub, .. } => {
                if has_pub && !is_pub {
                    None
                } else {
                    Some(name.clone())
                }
            }
            _ => None,
        })
        .collect()
}

fn scan_program_preflight(
    file_path: &Path,
    source: &str,
    program: &[SNode],
    config: &CheckConfig,
    host_capabilities: &HashMap<String, HashSet<String>>,
    visited: &mut HashSet<PathBuf>,
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    let canonical = file_path
        .canonicalize()
        .unwrap_or_else(|_| file_path.to_path_buf());
    if !visited.insert(canonical.clone()) {
        return;
    }
    for node in program {
        scan_node_preflight(
            node,
            &canonical,
            source,
            config,
            host_capabilities,
            visited,
            diagnostics,
        );
    }
}

fn scan_node_preflight(
    node: &SNode,
    file_path: &Path,
    source: &str,
    config: &CheckConfig,
    host_capabilities: &HashMap<String, HashSet<String>>,
    visited: &mut HashSet<PathBuf>,
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    match &node.node {
        Node::ImportDecl { path } | Node::SelectiveImport { path, .. } => {
            if path.starts_with("std/") {
                return;
            }
            match resolve_import_path(file_path, path) {
                Some(import_path) => {
                    let import_str = import_path.to_string_lossy().to_string();
                    let (import_source, import_program) = parse_source_file(&import_str);
                    scan_program_preflight(
                        &import_path,
                        &import_source,
                        &import_program,
                        config,
                        host_capabilities,
                        visited,
                        diagnostics,
                    );
                }
                None => diagnostics.push(PreflightDiagnostic {
                    path: file_path.display().to_string(),
                    source: source.to_string(),
                    span: node.span,
                    message: format!("preflight: unresolved import '{path}'"),
                    help: Some("verify the import path and packaged module layout".to_string()),
                }),
            }
        }
        Node::FunctionCall { name, args } if name == "render" => {
            if let Some(Node::StringLiteral(template_path)) = args.first().map(|arg| &arg.node) {
                let resolved = resolve_preflight_target(file_path, template_path, config);
                if !resolved.iter().any(|path| path.exists()) {
                    diagnostics.push(PreflightDiagnostic {
                        path: file_path.display().to_string(),
                        source: source.to_string(),
                        span: args[0].span,
                        message: format!(
                            "preflight: render target '{}' does not exist at {}",
                            template_path,
                            render_candidate_paths(&resolved)
                        ),
                        help: Some(
                            "keep template paths relative to the pipeline source file, or set [check].bundle_root / --bundle-root for bundled layouts"
                                .to_string(),
                        ),
                    });
                }
            }
        }
        Node::FunctionCall { name, args } if name == "exec_at" || name == "shell_at" => {
            if let Some(dir) = args.first().and_then(literal_string) {
                let resolved = resolve_source_relative(file_path, &dir);
                if !resolved.is_dir() {
                    diagnostics.push(PreflightDiagnostic {
                        path: file_path.display().to_string(),
                        source: source.to_string(),
                        span: args[0].span,
                        message: format!(
                            "preflight: execution directory '{}' does not exist at {}",
                            dir,
                            resolved.display()
                        ),
                        help: Some(
                            "use a source-relative directory that exists at preflight time, or create it before execution"
                                .to_string(),
                        ),
                    });
                }
            }
        }
        Node::FunctionCall { name, args } if name == "spawn_agent" => {
            if let Some(agent_config) = args.first() {
                scan_spawn_agent_preflight(agent_config, file_path, source, diagnostics);
            }
            scan_children(
                args,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::FunctionCall { name, args } if name == "host_invoke" => {
            diagnostics.push(PreflightDiagnostic {
                path: file_path.display().to_string(),
                source: source.to_string(),
                span: node.span,
                message: "preflight: host_invoke(...) was removed; use host_call(\"capability.operation\", args)".to_string(),
                help: Some(
                    "replace host_invoke(\"project\", \"scan\", {}) with host_call(\"project.scan\", {})"
                        .to_string(),
                ),
            });
            scan_children(
                args,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::FunctionCall { name, args } if name == "host_call" => {
            if let Some((cap, op, params_arg)) = parse_host_call_args(args) {
                if !is_known_host_operation(host_capabilities, &cap, &op) {
                    diagnostics.push(PreflightDiagnostic {
                        path: file_path.display().to_string(),
                        source: source.to_string(),
                        span: node.span,
                        message: format!(
                            "preflight: unknown host capability/operation '{cap}.{op}'"
                        ),
                        help: Some(
                            "declare additional host capabilities in [check].host_capabilities, [check].host_capabilities_path, or --host-capabilities"
                                .to_string(),
                        ),
                    });
                }
                if cap == "template" && op == "render" {
                    if let Some(template_path) = host_render_path_arg(params_arg) {
                        let resolved = resolve_preflight_target(file_path, &template_path, config);
                        if !resolved.iter().any(|path| path.exists()) {
                            diagnostics.push(PreflightDiagnostic {
                                path: file_path.display().to_string(),
                                source: source.to_string(),
                                span: params_arg.map(|arg| arg.span).unwrap_or(node.span),
                                message: format!(
                                    "preflight: host template render target '{}' does not exist at {}",
                                    template_path,
                                    render_candidate_paths(&resolved)
                                ),
                                help: Some(
                                    "verify the template path, or set [check].bundle_root / --bundle-root when validating bundled layouts"
                                        .to_string(),
                                ),
                            });
                        }
                    }
                }
            } else if let Some(arg) = args.first() {
                diagnostics.push(PreflightDiagnostic {
                    path: file_path.display().to_string(),
                    source: source.to_string(),
                    span: arg.span,
                    message: "preflight: host_call(...) requires a literal \"capability.operation\" name for static validation".to_string(),
                    help: Some(
                        "use a string literal like host_call(\"project.scan\", {}) so preflight can validate the capability contract"
                            .to_string(),
                    ),
                });
            }
            scan_children(
                args,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::IfElse {
            condition,
            then_body,
            else_body,
        } => {
            scan_node_preflight(
                condition,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_children(
                then_body,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            if let Some(else_body) = else_body {
                scan_children(
                    else_body,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
        }
        Node::ForIn { iterable, body, .. }
        | Node::WhileLoop {
            condition: iterable,
            body,
        } => {
            scan_node_preflight(
                iterable,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_children(
                body,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::Retry { count, body } => {
            scan_node_preflight(
                count,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_children(
                body,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::ReturnStmt { value } => {
            if let Some(value) = value {
                scan_node_preflight(
                    value,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
        }
        Node::RequireStmt { condition, message } => {
            scan_node_preflight(
                condition,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            if let Some(message) = message {
                scan_node_preflight(
                    message,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
        }
        Node::TryCatch {
            body,
            catch_body,
            finally_body,
            ..
        } => {
            scan_children(
                body,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_children(
                catch_body,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            if let Some(finally_body) = finally_body {
                scan_children(
                    finally_body,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
        }
        Node::TryExpr { body }
        | Node::SpawnExpr { body }
        | Node::MutexBlock { body }
        | Node::DeferStmt { body } => {
            scan_children(
                body,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::GuardStmt {
            condition,
            else_body,
        } => {
            scan_node_preflight(
                condition,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_children(
                else_body,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::DictLiteral(fields) => {
            for field in fields {
                scan_node_preflight(
                    &field.value,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
        }
        Node::DeadlineBlock { duration, body } => {
            scan_node_preflight(
                duration,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_children(
                body,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::YieldExpr { value } => {
            if let Some(value) = value {
                scan_node_preflight(
                    value,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
        }
        Node::Parallel { expr, body, .. } => {
            scan_node_preflight(
                expr,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_children(
                body,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::SelectExpr {
            cases,
            timeout,
            default_body,
        } => {
            for case in cases {
                scan_node_preflight(
                    &case.channel,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
                scan_children(
                    &case.body,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
            if let Some((timeout_expr, body)) = timeout {
                scan_node_preflight(
                    timeout_expr,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
                scan_children(
                    body,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
            if let Some(body) = default_body {
                scan_children(
                    body,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
        }
        Node::FunctionCall { args, .. } => {
            scan_children(
                args,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::MethodCall { object, args, .. } | Node::OptionalMethodCall { object, args, .. } => {
            scan_node_preflight(
                object,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_children(
                args,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::PropertyAccess { object, .. }
        | Node::OptionalPropertyAccess { object, .. }
        | Node::UnaryOp {
            operand: object, ..
        } => {
            scan_node_preflight(
                object,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::SubscriptAccess { object, index } => {
            scan_node_preflight(
                object,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_node_preflight(
                index,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::SliceAccess { object, start, end } => {
            scan_node_preflight(
                object,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            if let Some(start) = start {
                scan_node_preflight(
                    start,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
            if let Some(end) = end {
                scan_node_preflight(
                    end,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
        }
        Node::BinaryOp { left, right, .. } => {
            scan_node_preflight(
                left,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_node_preflight(
                right,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => {
            scan_node_preflight(
                condition,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_node_preflight(
                true_expr,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_node_preflight(
                false_expr,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::Assignment { target, value, .. } => {
            scan_node_preflight(
                target,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_node_preflight(
                value,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::ThrowStmt { value } => {
            scan_node_preflight(
                value,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::EnumConstruct { args, .. } | Node::ListLiteral(args) => {
            scan_children(
                args,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::StructConstruct { fields, .. } => {
            for field in fields {
                scan_node_preflight(
                    &field.value,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
        }
        Node::RangeExpr { start, end, .. } => {
            scan_node_preflight(
                start,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_node_preflight(
                end,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::Pipeline { body, .. }
        | Node::OverrideDecl { body, .. }
        | Node::FnDecl { body, .. }
        | Node::ToolDecl { body, .. } => {
            scan_children(
                body,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::LetBinding { value, .. } | Node::VarBinding { value, .. } => {
            scan_node_preflight(
                value,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::MatchExpr { value, arms } => {
            scan_node_preflight(
                value,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            for arm in arms {
                scan_children(
                    &arm.body,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
                scan_node_preflight(
                    &arm.pattern,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
        }
        Node::ImplBlock { methods, .. } => {
            scan_children(
                methods,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::Spread(expr) | Node::TryOperator { operand: expr } => {
            scan_node_preflight(
                expr,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::Block(body) | Node::Closure { body, .. } => {
            scan_children(
                body,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::TypeDecl { .. }
        | Node::EnumDecl { .. }
        | Node::StructDecl { .. }
        | Node::InterfaceDecl { .. }
        | Node::DurationLiteral(_)
        | Node::InterpolatedString(_)
        | Node::StringLiteral(_)
        | Node::RawStringLiteral(_)
        | Node::IntLiteral(_)
        | Node::FloatLiteral(_)
        | Node::BoolLiteral(_)
        | Node::NilLiteral
        | Node::Identifier(_)
        | Node::BreakStmt
        | Node::ContinueStmt => {}
    }
}

fn scan_children(
    nodes: &[SNode],
    file_path: &Path,
    source: &str,
    config: &CheckConfig,
    host_capabilities: &HashMap<String, HashSet<String>>,
    visited: &mut HashSet<PathBuf>,
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    for node in nodes {
        scan_node_preflight(
            node,
            file_path,
            source,
            config,
            host_capabilities,
            visited,
            diagnostics,
        );
    }
}

fn resolve_import_path(current_file: &Path, import_path: &str) -> Option<PathBuf> {
    let base = current_file.parent().unwrap_or(Path::new("."));
    let mut file_path = base.join(import_path);
    if !file_path.exists() && file_path.extension().is_none() {
        file_path.set_extension("harn");
    }
    if file_path.exists() {
        return Some(file_path);
    }
    {
        let pkg_path = base.join(".harn/packages").join(import_path);
        if pkg_path.exists() {
            return Some(if pkg_path.is_dir() {
                let lib = pkg_path.join("lib.harn");
                if lib.exists() {
                    lib
                } else {
                    pkg_path
                }
            } else {
                pkg_path
            });
        }
        let mut pkg_harn = pkg_path.clone();
        pkg_harn.set_extension("harn");
        if pkg_harn.exists() {
            return Some(pkg_harn);
        }
    }
    None
}

fn resolve_source_relative(current_file: &Path, target: &str) -> PathBuf {
    let candidate = PathBuf::from(target);
    if candidate.is_absolute() {
        candidate
    } else {
        current_file
            .parent()
            .unwrap_or(Path::new("."))
            .join(candidate)
    }
}

fn resolve_preflight_target(
    current_file: &Path,
    target: &str,
    config: &CheckConfig,
) -> Vec<PathBuf> {
    let mut candidates = vec![resolve_source_relative(current_file, target)];
    if let Some(bundle_root) = config.bundle_root.as_deref() {
        let bundle_base = PathBuf::from(bundle_root);
        candidates.push(if PathBuf::from(target).is_absolute() {
            PathBuf::from(target)
        } else {
            bundle_base.join(target)
        });
    }
    candidates.dedup();
    candidates
}

fn render_candidate_paths(candidates: &[PathBuf]) -> String {
    candidates
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(" or ")
}

fn default_host_capabilities() -> HashMap<String, HashSet<String>> {
    HashMap::from([
        (
            "workspace".to_string(),
            HashSet::from([
                "read_text".to_string(),
                "write_text".to_string(),
                "apply_edit".to_string(),
                "delete".to_string(),
                "exists".to_string(),
                "file_exists".to_string(),
                "list".to_string(),
                "project_root".to_string(),
                "roots".to_string(),
            ]),
        ),
        ("process".to_string(), HashSet::from(["exec".to_string()])),
        (
            "template".to_string(),
            HashSet::from(["render".to_string()]),
        ),
        (
            "interaction".to_string(),
            HashSet::from(["ask".to_string()]),
        ),
        (
            "runtime".to_string(),
            HashSet::from([
                "approved_plan".to_string(),
                "dry_run".to_string(),
                "pipeline_input".to_string(),
                "record_run".to_string(),
                "set_result".to_string(),
                "task".to_string(),
            ]),
        ),
        (
            "project".to_string(),
            HashSet::from([
                "agent_instructions".to_string(),
                "code_patterns".to_string(),
                "compute_content_hash".to_string(),
                "ide_context".to_string(),
                "lessons".to_string(),
                "mcp_config".to_string(),
                "metadata_get".to_string(),
                "metadata_refresh_hashes".to_string(),
                "metadata_save".to_string(),
                "metadata_set".to_string(),
                "metadata_stale".to_string(),
                "scan".to_string(),
                "scope_test_command".to_string(),
                "test_commands".to_string(),
            ]),
        ),
        (
            "session".to_string(),
            HashSet::from([
                "active_roots".to_string(),
                "changed_paths".to_string(),
                "preread_get".to_string(),
                "preread_read_many".to_string(),
            ]),
        ),
        (
            "editor".to_string(),
            HashSet::from([
                "get_active_file".to_string(),
                "get_selection".to_string(),
                "get_visible_files".to_string(),
            ]),
        ),
        (
            "diagnostics".to_string(),
            HashSet::from(["get_causal_traces".to_string(), "get_errors".to_string()]),
        ),
        (
            "git".to_string(),
            HashSet::from(["get_branch".to_string(), "get_diff".to_string()]),
        ),
        (
            "learning".to_string(),
            HashSet::from([
                "get_learned_rules".to_string(),
                "report_correction".to_string(),
            ]),
        ),
    ])
}

fn merge_host_capability_map(
    target: &mut HashMap<String, HashSet<String>>,
    source: HashMap<String, HashSet<String>>,
) {
    for (capability, ops) in source {
        target.entry(capability).or_default().extend(ops);
    }
}

fn parse_host_capability_value(value: &serde_json::Value) -> HashMap<String, HashSet<String>> {
    let root = value.get("capabilities").unwrap_or(value);
    let mut result = HashMap::new();
    let Some(capabilities) = root.as_object() else {
        return result;
    };
    for (capability, entry) in capabilities {
        let mut ops = HashSet::new();
        if let Some(list) = entry.as_array() {
            for item in list {
                if let Some(op) = item.as_str() {
                    ops.insert(op.to_string());
                }
            }
        } else if let Some(obj) = entry.as_object() {
            if let Some(list) = obj
                .get("operations")
                .or_else(|| obj.get("ops"))
                .and_then(|v| v.as_array())
            {
                for item in list {
                    if let Some(op) = item.as_str() {
                        ops.insert(op.to_string());
                    }
                }
            } else {
                for (op, enabled) in obj {
                    if enabled.as_bool().unwrap_or(true) {
                        ops.insert(op.to_string());
                    }
                }
            }
        }
        if !ops.is_empty() {
            result.insert(capability.to_string(), ops);
        }
    }
    result
}

pub(crate) fn load_host_capabilities(config: &CheckConfig) -> HashMap<String, HashSet<String>> {
    let mut capabilities = default_host_capabilities();
    let inline = config
        .host_capabilities
        .iter()
        .map(|(capability, ops)| {
            (
                capability.clone(),
                ops.iter().cloned().collect::<HashSet<String>>(),
            )
        })
        .collect::<HashMap<_, _>>();
    merge_host_capability_map(&mut capabilities, inline);
    if let Some(path) = config.host_capabilities_path.as_deref() {
        if let Ok(content) = std::fs::read_to_string(path) {
            let parsed_json = serde_json::from_str::<serde_json::Value>(&content).ok();
            let parsed_toml = toml::from_str::<toml::Value>(&content)
                .ok()
                .and_then(|value| serde_json::to_value(value).ok());
            if let Some(value) = parsed_json.or(parsed_toml) {
                merge_host_capability_map(&mut capabilities, parse_host_capability_value(&value));
            }
        }
    }
    capabilities
}

fn is_known_host_operation(
    capabilities: &HashMap<String, HashSet<String>>,
    capability: &str,
    operation: &str,
) -> bool {
    capabilities
        .get(capability)
        .is_some_and(|ops| ops.contains(operation))
}

fn host_render_path_arg(arg: Option<&SNode>) -> Option<String> {
    let Node::DictLiteral(entries) = &arg?.node else {
        return None;
    };
    entries
        .iter()
        .find_map(|entry| match (&entry.key.node, &entry.value.node) {
            (Node::Identifier(key), Node::StringLiteral(path)) if key == "path" => {
                Some(path.clone())
            }
            (Node::StringLiteral(key), Node::StringLiteral(path)) if key == "path" => {
                Some(path.clone())
            }
            _ => None,
        })
}

fn parse_host_call_args(args: &[SNode]) -> Option<(String, String, Option<&SNode>)> {
    let Node::StringLiteral(name) = &args.first()?.node else {
        return None;
    };
    let (capability, operation) = name.split_once('.')?;
    Some((capability.to_string(), operation.to_string(), args.get(1)))
}

fn literal_string(node: &SNode) -> Option<String> {
    match &node.node {
        Node::StringLiteral(value) => Some(value.clone()),
        _ => None,
    }
}

fn dict_literal_field<'a>(node: &'a SNode, field: &str) -> Option<&'a SNode> {
    let Node::DictLiteral(entries) = &node.node else {
        return None;
    };
    entries.iter().find_map(|entry| match &entry.key.node {
        Node::Identifier(key) | Node::StringLiteral(key) if key == field => Some(&entry.value),
        _ => None,
    })
}

fn scan_spawn_agent_preflight(
    config: &SNode,
    file_path: &Path,
    source: &str,
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    let Some(execution) = dict_literal_field(config, "execution") else {
        return;
    };
    if let Some(cwd) = dict_literal_field(execution, "cwd").and_then(literal_string) {
        let resolved = resolve_source_relative(file_path, &cwd);
        if !resolved.is_dir() {
            diagnostics.push(PreflightDiagnostic {
                path: file_path.display().to_string(),
                source: source.to_string(),
                span: execution.span,
                message: format!(
                    "preflight: worker execution cwd '{}' does not exist at {}",
                    cwd,
                    resolved.display()
                ),
                help: Some(
                    "keep literal worker cwd paths source-relative and valid, or switch to a worktree adapter"
                        .to_string(),
                ),
            });
        }
    }
    let Some(worktree) = dict_literal_field(execution, "worktree") else {
        return;
    };
    if let Some(repo) = dict_literal_field(worktree, "repo").and_then(literal_string) {
        let resolved = resolve_source_relative(file_path, &repo);
        if !resolved.is_dir() {
            diagnostics.push(PreflightDiagnostic {
                path: file_path.display().to_string(),
                source: source.to_string(),
                span: worktree.span,
                message: format!(
                    "preflight: worker worktree repo '{}' does not exist at {}",
                    repo,
                    resolved.display()
                ),
                help: Some(
                    "point worktree.repo at a real git checkout so isolated execution can be prepared"
                        .to_string(),
                ),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_lexer::Lexer;
    use harn_parser::Parser;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn parse_program(source: &str) -> Vec<SNode> {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize().expect("tokenize");
        let mut parser = Parser::new(tokens);
        parser.parse().expect("parse")
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{nanos}"))
    }

    #[test]
    fn preflight_reports_missing_literal_render_target() {
        let dir = unique_temp_dir("harn-check");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.harn");
        let source = r#"
pipeline main() {
  let text = render("missing.txt")
  println(text)
}
"#;
        let program = parse_program(source);
        let diagnostics =
            collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("render target"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preflight_resolves_imports_with_implicit_harn_extension() {
        let dir = unique_temp_dir("harn-check");
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("lib").join("helpers.harn"), "pub fn x() { 1 }\n").unwrap();
        let file = dir.join("main.harn");
        let resolved = resolve_import_path(&file, "lib/helpers");
        assert_eq!(resolved, Some(dir.join("lib").join("helpers.harn")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preflight_reports_missing_worker_execution_repo() {
        let dir = unique_temp_dir("harn-check-worker");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.harn");
        let source = r#"
pipeline main() {
  spawn_agent({
    task: "do it",
    node: {kind: "stage"},
    execution: {worktree: {repo: "./missing-repo"}}
  })
}
"#;
        let program = parse_program(source);
        let diagnostics =
            collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("worktree repo"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preflight_detects_import_collision() {
        let dir = unique_temp_dir("harn-check-collision");
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("lib").join("a.harn"), "pub fn helper() { 1 }\n").unwrap();
        std::fs::write(dir.join("lib").join("b.harn"), "pub fn helper() { 2 }\n").unwrap();
        let file = dir.join("main.harn");
        let source = r#"
import "lib/a.harn"
import "lib/b.harn"

pipeline main() {
  log(helper())
}
"#;
        let program = parse_program(source);
        let diagnostics =
            collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
        assert!(
            diagnostics
                .iter()
                .any(|d| d.message.contains("import collision")),
            "expected import collision diagnostic, got: {:?}",
            diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preflight_no_collision_with_selective_imports() {
        let dir = unique_temp_dir("harn-check-selective");
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(
            dir.join("lib").join("a.harn"),
            "pub fn foo() { 1 }\npub fn shared() { 2 }\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("lib").join("b.harn"),
            "pub fn bar() { 3 }\npub fn shared() { 4 }\n",
        )
        .unwrap();
        let file = dir.join("main.harn");
        let source = r#"
import { foo } from "lib/a.harn"
import { bar } from "lib/b.harn"

pipeline main() {
  log(foo())
  log(bar())
}
"#;
        let program = parse_program(source);
        let diagnostics =
            collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
        assert!(
            diagnostics
                .iter()
                .all(|d| !d.message.contains("import collision")),
            "unexpected collision: {:?}",
            diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preflight_reports_unknown_host_capability() {
        let dir = unique_temp_dir("harn-check-host");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.harn");
        let source = r#"
pipeline main() {
  host_call("unknown_cap.do_stuff", {})
}
"#;
        let program = parse_program(source);
        let diagnostics =
            collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
        assert!(
            diagnostics
                .iter()
                .any(|d| d.message.contains("unknown host capability")),
            "expected unknown host capability diagnostic, got: {:?}",
            diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preflight_accepts_known_host_capabilities() {
        let dir = unique_temp_dir("harn-check-host-ok");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.harn");
        let source = r#"
pipeline main() {
  host_call("project.metadata_get", {dir: ".", namespace: "facts"})
  host_call("process.exec", {command: "ls"})
}
"#;
        let program = parse_program(source);
        let diagnostics =
            collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
        assert!(
            diagnostics
                .iter()
                .all(|d| !d.message.contains("unknown host capability")),
            "unexpected host cap diagnostic: {:?}",
            diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preflight_accepts_extended_host_capabilities_from_config() {
        let dir = unique_temp_dir("harn-check-host-extended");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.harn");
        let source = r#"
pipeline main() {
  host_call("project.scan", {})
  host_call("runtime.set_result", {})
}
"#;
        let program = parse_program(source);
        let diagnostics = collect_preflight_diagnostics(
            &file,
            source,
            &program,
            &CheckConfig {
                host_capabilities: HashMap::from([
                    ("project".to_string(), vec!["scan".to_string()]),
                    ("runtime".to_string(), vec!["set_result".to_string()]),
                ]),
                ..CheckConfig::default()
            },
        );
        assert!(
            diagnostics
                .iter()
                .all(|d| !d.message.contains("unknown host capability")),
            "unexpected host cap diagnostic: {:?}",
            diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preflight_accepts_runtime_task_and_session_ops() {
        let dir = unique_temp_dir("harn-check-host-runtime");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.harn");
        let source = r#"
pipeline main() {
  host_call("runtime.task", {})
  host_call("session.changed_paths", {})
}
"#;
        let program = parse_program(source);
        let diagnostics =
            collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
        assert!(
            diagnostics
                .iter()
                .all(|d| !d.message.contains("unknown host capability")),
            "unexpected host cap diagnostic: {:?}",
            diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preflight_accepts_host_operations_registered_via_host_mock() {
        let dir = unique_temp_dir("harn-check-host-mock");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.harn");
        let source = r#"
pipeline main() {
  host_mock("project", "metadata_get", {result: {value: "facts"}})
  host_call("project.metadata_get", {dir: "pkg"})
}
"#;
        let program = parse_program(source);
        let diagnostics =
            collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
        assert!(
            diagnostics
                .iter()
                .all(|d| !d.message.contains("unknown host capability")),
            "unexpected host cap diagnostic: {:?}",
            diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_harn_targets_recurses_directories_and_deduplicates() {
        let dir = unique_temp_dir("harn-check-targets");
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::write(dir.join("a.harn"), "pipeline a() {}\n").unwrap();
        std::fs::write(dir.join("nested").join("b.harn"), "pipeline b() {}\n").unwrap();
        std::fs::write(dir.join("nested").join("ignore.txt"), "x\n").unwrap();

        let target_dir = dir.display().to_string();
        let target_file = dir.join("a.harn").display().to_string();
        let files = collect_harn_targets(&[target_dir.as_str(), target_file.as_str()]);

        assert_eq!(files.len(), 2);
        assert!(files.contains(&dir.join("a.harn")));
        assert!(files.contains(&dir.join("nested").join("b.harn")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_host_capability_value_accepts_top_level_object_schema() {
        let value = serde_json::json!({
            "workspace": ["project_root", "file_exists"],
            "runtime": {
                "operations": ["task", "pipeline_input"]
            }
        });
        let parsed = parse_host_capability_value(&value);
        assert!(parsed["workspace"].contains("project_root"));
        assert!(parsed["workspace"].contains("file_exists"));
        assert!(parsed["runtime"].contains("task"));
        assert!(parsed["runtime"].contains("pipeline_input"));
    }

    #[test]
    fn preflight_accepts_render_target_from_bundle_root() {
        let dir = unique_temp_dir("harn-check-bundle-root");
        std::fs::create_dir_all(dir.join("bundle")).unwrap();
        std::fs::write(dir.join("bundle").join("shared.prompt"), "hello").unwrap();
        let file = dir.join("main.harn");
        let source = r#"
pipeline main() {
  let text = render("shared.prompt")
  println(text)
}
"#;
        let program = parse_program(source);
        let diagnostics = collect_preflight_diagnostics(
            &file,
            source,
            &program,
            &CheckConfig {
                bundle_root: Some(dir.join("bundle").display().to_string()),
                ..CheckConfig::default()
            },
        );
        assert!(
            diagnostics
                .iter()
                .all(|d| !d.message.contains("render target")),
            "unexpected render diagnostic: {:?}",
            diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preflight_validates_render_in_imported_module() {
        let dir = unique_temp_dir("harn-check-import-render");
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        // Module references a template that doesn't exist
        std::fs::write(
            dir.join("lib").join("tmpl.harn"),
            "pub fn load() { render(\"missing_template.txt\") }\n",
        )
        .unwrap();
        let file = dir.join("main.harn");
        let source = r#"
import "lib/tmpl.harn"

pipeline main() {
  log(load())
}
"#;
        let program = parse_program(source);
        let diagnostics =
            collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
        assert!(
            diagnostics
                .iter()
                .any(|d| d.message.contains("render target")),
            "expected render target diagnostic for imported module, got: {:?}",
            diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bundle_manifest_tracks_prompt_assets_host_caps_and_worktree_repos() {
        let dir = unique_temp_dir("harn-check-bundle-manifest");
        std::fs::create_dir_all(dir.join("prompts")).unwrap();
        std::fs::create_dir_all(dir.join("shared")).unwrap();
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("prompts").join("review.harn.prompt"), "review").unwrap();
        std::fs::write(dir.join("shared").join("snippet.prompt"), "snippet").unwrap();
        std::fs::write(
            dir.join("lib").join("helper.harn"),
            r#"
pub fn helper() -> string {
  return "ok"
}
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("main.harn"),
            r#"
import "lib/helper.harn"

pipeline main() {
  let review = render_prompt("prompts/review.harn.prompt")
  let snippet = render("shared/snippet.prompt")
  host_call("project.scan", {})
  exec_at("shared", "pwd")
  spawn_agent({
    task: "scan",
    node: {kind: "stage"},
    execution: {worktree: {repo: "./repo"}}
  })
  println(review + snippet)
}
"#,
        )
        .unwrap();
        let manifest = build_bundle_manifest(&[dir.join("main.harn")], &CheckConfig::default());
        assert_eq!(
            manifest["entry_modules"].as_array().map(|v| v.len()),
            Some(1)
        );
        assert_eq!(
            manifest["import_modules"].as_array().map(|v| v.len()),
            Some(1)
        );
        assert!(manifest["module_dependencies"]
            .as_array()
            .expect("module dependencies")
            .iter()
            .any(|edge| edge["from"]
                .as_str()
                .is_some_and(|value| value.ends_with("/main.harn"))
                && edge["to"]
                    .as_str()
                    .is_some_and(|value| value.ends_with("/lib/helper.harn"))));
        let assets = manifest["assets"].as_array().expect("assets array");
        assert!(assets.iter().any(|asset| {
            asset["kind"] == "prompt_asset"
                && asset["via"] == "render_prompt"
                && asset["target"] == "prompts/review.harn.prompt"
        }));
        assert!(assets.iter().any(|asset| {
            asset["kind"] == "prompt_asset"
                && asset["via"] == "render"
                && asset["target"] == "shared/snippet.prompt"
        }));
        assert!(manifest["prompt_assets"]
            .as_array()
            .expect("prompt assets")
            .iter()
            .any(|entry| entry
                .as_str()
                .is_some_and(|value| value.ends_with("/prompts/review.harn.prompt"))));
        assert!(manifest["prompt_assets"]
            .as_array()
            .expect("prompt assets")
            .iter()
            .any(|entry| entry
                .as_str()
                .is_some_and(|value| value.ends_with("/shared/snippet.prompt"))));
        assert_eq!(manifest["summary"]["prompt_asset_count"].as_u64(), Some(2));
        assert_eq!(
            manifest["summary"]["module_dependency_count"].as_u64(),
            Some(1)
        );
        assert_eq!(manifest["required_host_capabilities"]["project"][0], "scan");
        assert!(manifest["execution_dirs"]
            .as_array()
            .expect("execution dirs")
            .iter()
            .any(|entry| entry
                .as_str()
                .is_some_and(|value| value.ends_with("/shared"))));
        assert!(manifest["worktree_repos"]
            .as_array()
            .expect("worktree repos")
            .iter()
            .any(|entry| entry.as_str().is_some_and(|value| value.ends_with("/repo"))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_lint_reports_missing_harndoc_for_public_functions() {
        let source = r#"
pub fn exposed() -> string {
  return "x"
}
"#;
        let program = parse_program(source);
        let diagnostics = harn_lint::lint_with_config_and_source(
            &program,
            &CheckConfig::default().disable_rules,
            Some(source),
        );
        assert!(
            diagnostics.iter().any(|d| d.rule == "missing-harndoc"),
            "expected missing-harndoc warning, got: {:?}",
            diagnostics.iter().map(|d| &d.rule).collect::<Vec<_>>()
        );
    }
}
