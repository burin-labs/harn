use std::collections::{HashMap, HashSet};
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
        Node::AskExpr { fields } | Node::DictLiteral(fields) => {
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
        Node::Parallel { count, body, .. } => {
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
        Node::ParallelMap { list, body, .. } | Node::ParallelSettle { list, body, .. } => {
            collect_mock_host_capabilities_from_node(
                list,
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
