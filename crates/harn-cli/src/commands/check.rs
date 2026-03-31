use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process;

use harn_fmt::format_source;
use harn_lint::LintSeverity;
use harn_parser::{DiagnosticSeverity, Node, SNode, TypeChecker};

use crate::package::CheckConfig;
use crate::parse_source_file;

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

pub(crate) fn check_file(path: &str, config: &CheckConfig) {
    let (source, program) = parse_source_file(path);

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
                path,
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
    let lint_diagnostics =
        harn_lint::lint_with_config_and_source(&program, &config.disable_rules, Some(&source));
    diagnostic_count += lint_diagnostics.len();
    if lint_diagnostics
        .iter()
        .any(|d| d.severity == LintSeverity::Warning)
    {
        has_warning = true;
    }
    if print_lint_diagnostics(path, &lint_diagnostics) {
        has_error = true;
    }

    let preflight_diagnostics =
        collect_preflight_diagnostics(Path::new(path), &source, &program, config);
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
        println!("{path}: ok");
    }

    if has_error || (config.strict && has_warning) {
        process::exit(1);
    }
}

pub(crate) fn lint_file(path: &str, config: &CheckConfig) {
    let (source, program) = parse_source_file(path);

    let diagnostics =
        harn_lint::lint_with_config_and_source(&program, &config.disable_rules, Some(&source));

    if diagnostics.is_empty() {
        println!("{path}: no issues found");
        return;
    }

    let has_warning = diagnostics
        .iter()
        .any(|d| d.severity == LintSeverity::Warning);
    let has_error = print_lint_diagnostics(path, &diagnostics);

    if has_error || (config.strict && has_warning) {
        process::exit(1);
    }
}

/// Format one or more files or directories. Accepts multiple targets.
pub(crate) fn fmt_targets(targets: &[&str], check_mode: bool) {
    let mut files = Vec::new();
    for target in targets {
        let path = std::path::Path::new(target);
        if path.is_dir() {
            collect_harn_files(path, &mut files);
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
        if !fmt_file_inner(&path_str, check_mode) {
            has_error = true;
        }
    }
    if has_error {
        process::exit(1);
    }
}

/// Recursively collect .harn files in a directory.
fn collect_harn_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.path());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                collect_harn_files(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "harn") {
                out.push(path);
            }
        }
    }
}

/// Format a single file. Returns true on success, false on error.
fn fmt_file_inner(path: &str, check_mode: bool) -> bool {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error reading {path}: {e}");
            return false;
        }
    };

    let formatted = match format_source(&source) {
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
    let host_capabilities = load_host_capabilities(config);
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
            // Validate known capability/operation pairs
            if let (Some(Node::StringLiteral(cap)), Some(Node::StringLiteral(op))) =
                (args.first().map(|a| &a.node), args.get(1).map(|a| &a.node))
            {
                if !is_known_host_operation(host_capabilities, cap, op) {
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
                // Template render target check
                if cap == "template" && op == "render" {
                    if let Some(template_path) = host_render_path_arg(args.get(2)) {
                        let resolved = resolve_preflight_target(file_path, &template_path, config);
                        if !resolved.iter().any(|path| path.exists()) {
                            diagnostics.push(PreflightDiagnostic {
                                path: file_path.display().to_string(),
                                source: source.to_string(),
                                span: args[2].span,
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
        Node::TryExpr { body } | Node::SpawnExpr { body } | Node::MutexBlock { body } => {
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
        Node::AskExpr { fields } | Node::DictLiteral(fields) => {
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
        Node::Parallel { count, body, .. } => {
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
        Node::ParallelMap { list, body, .. } | Node::ParallelSettle { list, body, .. } => {
            scan_node_preflight(
                list,
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
        | Node::FnDecl { body, .. } => {
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
    for pkg_dir in [".harn/packages", ".burin/packages"] {
        let pkg_path = base.join(pkg_dir).join(import_path);
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
                "list".to_string(),
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

fn load_host_capabilities(config: &CheckConfig) -> HashMap<String, HashSet<String>> {
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
  host_invoke("unknown_cap", "do_stuff", {})
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
  host_invoke("workspace", "read_text", {path: "x.txt"})
  host_invoke("process", "exec", {command: "ls"})
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
  host_invoke("project", "scan", {})
  host_invoke("runtime", "set_result", {})
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
