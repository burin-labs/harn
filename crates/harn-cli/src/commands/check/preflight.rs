use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use harn_modules::resolve_import_path;
use harn_parser::{DiagnosticCode as Code, Node, SNode};

use super::host_capabilities::{is_known_host_operation, HostCapabilities};
use super::imports::{
    scan_import_collisions, scan_re_export_conflicts, scan_selective_import_visibility,
};
use super::mock_host::collect_mock_host_capabilities;
use super::result_cache::Probe;
use super::scan_memo::ScanMemo;
use super::source::parse_resolved_module;
use crate::package::CheckConfig;

mod effect_inheritance;
mod execution_target;
mod host_param_discriminators;
mod llm_composition;
mod static_tool_surface;
mod targets;
mod tool_scans;

use effect_inheritance::*;
use execution_target::{is_process_execution_method, scan_execution_dir_preflight};
use host_param_discriminators::scan_host_param_discriminators;
pub(super) use host_param_discriminators::{host_render_path_arg, parse_host_call_args};
use static_tool_surface::*;
use targets::*;
pub(super) use targets::{
    find_unique_basename, literal_string, resolve_preflight_target, resolve_source_relative,
};
pub(super) use tool_scans::dict_literal_field;
use tool_scans::*;

#[derive(Clone, Debug)]
pub(crate) struct PreflightDiagnostic {
    pub(crate) code: Code,
    pub(crate) path: String,
    pub(crate) source: String,
    pub(crate) span: harn_lexer::Span,
    pub(crate) message: String,
    pub(crate) help: Option<String>,
    /// Optional `"capability.operation"` tag for `[check].preflight_allow`.
    pub(crate) tags: Option<String>,
}

/// Returns whether `tag` matches an exact, capability-wide, or global allow entry.
pub(crate) fn is_preflight_allowed(tag: &Option<String>, allow: &[String]) -> bool {
    let Some(tag) = tag else { return false };
    let (cap, _) = tag.split_once('.').unwrap_or((tag.as_str(), ""));
    allow.iter().any(|entry| {
        let entry = entry.trim();
        if entry == "*" || entry == tag {
            return true;
        }
        if let Some(prefix) = entry.strip_suffix(".*") {
            return prefix == cap;
        }
        entry == cap
    })
}

/// What a file's preflight reads beyond the file and its config: the
/// resolved host capability surface and the check run's shared module-scan
/// memo.
#[derive(Clone, Copy)]
pub(crate) struct PreflightRun<'a> {
    pub(crate) host_capabilities: &'a HostCapabilities,
    pub(crate) memo: &'a ScanMemo,
}

pub(super) fn collect_preflight_diagnostics_with_host_capabilities(
    path: &Path,
    source: &str,
    program: &[SNode],
    config: &CheckConfig,
    module_graph: &harn_modules::ModuleGraph,
    configured_host_capabilities: &HostCapabilities,
    memo: &ScanMemo,
) -> Vec<PreflightDiagnostic> {
    let mut diagnostics = Vec::new();
    let canonical = harn_modules::canonical_path(path);
    let mut mocked = HashMap::new();
    collect_mock_host_capabilities(&canonical, source, program, memo, &mut mocked);
    let with_mocks;
    let host_capabilities = if mocked.is_empty() {
        configured_host_capabilities
    } else {
        let mut merged = configured_host_capabilities.clone();
        for (capability, operations) in mocked {
            merged
                .operations_mut()
                .entry(capability)
                .or_default()
                .extend(operations);
        }
        with_mocks = merged;
        &with_mocks
    };
    scan_program_preflight(
        &canonical,
        source,
        program,
        config,
        host_capabilities,
        memo,
        &mut diagnostics,
    );
    scan_import_collisions(&canonical, source, program, module_graph, &mut diagnostics);
    scan_selective_import_visibility(&canonical, source, module_graph, &mut diagnostics);
    scan_re_export_conflicts(&canonical, source, program, module_graph, &mut diagnostics);
    scan_static_tool_surface_preflight(&canonical, source, program, config, &mut diagnostics);
    llm_composition::scan_llm_capability_composition_preflight(
        &canonical,
        source,
        program,
        &mut diagnostics,
    );
    scan_effect_inheritance_preflight(&canonical, source, program, &mut diagnostics);
    diagnostics
}

/// One module's own preflight scan, in walk order. Diagnostics and recorded
/// filesystem probes are grouped into runs split at each import, so replaying
/// the events and following each import in place reproduces the depth-first
/// walk's diagnostic order and probe log exactly.
pub(super) struct ModuleScan {
    events: Vec<ScanEvent>,
}

enum ScanEvent {
    Diagnostics(Vec<PreflightDiagnostic>),
    Probes(Vec<Probe>),
    Import { path: PathBuf, canonical: PathBuf },
}

/// Walk the entry file and, through the run's memo, every module it imports.
/// Each imported module is scanned at most once per `(module, context)` per
/// run; later files replay its events, including its probes, into their own
/// diagnostics and probe log.
fn scan_program_preflight(
    file_path: &Path,
    source: &str,
    program: &[SNode],
    config: &CheckConfig,
    host_capabilities: &HostCapabilities,
    memo: &ScanMemo,
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    let canonical = harn_modules::canonical_path(file_path);
    let context = scan_context(config, host_capabilities);
    let mut visited = HashSet::from([canonical.clone()]);
    let entry = scan_module(&canonical, source, program, config, host_capabilities);
    let mut replay = Replay {
        config,
        host_capabilities,
        memo,
        context,
        visited: &mut visited,
        diagnostics,
    };
    replay.events(&entry);
}

/// The inputs besides the module itself that its preflight events read.
fn scan_context(config: &CheckConfig, host_capabilities: &HostCapabilities) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(super::result_cache::check_config_digest(config));
    hasher.update(host_capabilities.fingerprint());
    hasher.finalize().into()
}

fn scan_module(
    canonical: &Path,
    source: &str,
    program: &[SNode],
    config: &CheckConfig,
    host_capabilities: &HostCapabilities,
) -> ModuleScan {
    let (mut events, probes) = super::result_cache::capture_probes(|| {
        let mut events = Vec::new();
        let mut diagnostics = Vec::new();
        harn_parser::visit::walk_program_unattributed(program, &mut |node| {
            scan_node_preflight(
                node,
                canonical,
                source,
                config,
                host_capabilities,
                &mut events,
                &mut diagnostics,
            );
        });
        if !diagnostics.is_empty() {
            events.push(ScanEvent::Diagnostics(diagnostics));
        }
        events
    });
    if !probes.is_empty() {
        events.push(ScanEvent::Probes(probes));
    }
    ModuleScan { events }
}

/// Close the current diagnostic and probe runs and record an import to follow.
fn push_import(
    events: &mut Vec<ScanEvent>,
    diagnostics: &mut Vec<PreflightDiagnostic>,
    path: PathBuf,
) {
    if !diagnostics.is_empty() {
        events.push(ScanEvent::Diagnostics(std::mem::take(diagnostics)));
    }
    let probes = super::result_cache::drain_captured_probes();
    if !probes.is_empty() {
        events.push(ScanEvent::Probes(probes));
    }
    let canonical = harn_modules::canonical_path(&path);
    events.push(ScanEvent::Import { path, canonical });
}

struct Replay<'a> {
    config: &'a CheckConfig,
    host_capabilities: &'a HostCapabilities,
    memo: &'a ScanMemo,
    context: [u8; 32],
    visited: &'a mut HashSet<PathBuf>,
    diagnostics: &'a mut Vec<PreflightDiagnostic>,
}

impl Replay<'_> {
    fn events(&mut self, scan: &ModuleScan) {
        for event in &scan.events {
            match event {
                ScanEvent::Diagnostics(diagnostics) => {
                    self.diagnostics.extend(diagnostics.iter().cloned());
                }
                ScanEvent::Probes(probes) => super::result_cache::replay_probes(probes),
                ScanEvent::Import { path, canonical } => {
                    if self.visited.contains(canonical) {
                        continue;
                    }
                    let (config, host_capabilities) = (self.config, self.host_capabilities);
                    let imported = self.memo.preflight(canonical, self.context, || {
                        let parsed = parse_resolved_module(path)?;
                        Some(scan_module(
                            canonical,
                            &parsed.source,
                            &parsed.program,
                            config,
                            host_capabilities,
                        ))
                    });
                    if let Some(imported) = imported {
                        self.visited.insert(canonical.clone());
                        self.events(&imported);
                    }
                }
            }
        }
    }
}

fn scan_node_preflight(
    node: &SNode,
    file_path: &Path,
    source: &str,
    config: &CheckConfig,
    host_capabilities: &HostCapabilities,
    events: &mut Vec<ScanEvent>,
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    match &node.node {
        Node::ImportDecl { path, .. }
        | Node::SelectiveImport { path, .. }
        | Node::NamespaceImport { path, .. } => match resolve_import_path(file_path, path) {
            Some(import_path) => push_import(events, diagnostics, import_path),
            None => diagnostics.push(PreflightDiagnostic {
                code: Code::ModuleImportUnresolved,
                path: file_path.display().to_string(),
                source: source.to_string(),
                span: node.span,
                message: format!("preflight: unresolved import '{path}'"),
                help: Some("verify the import path and packaged module layout".to_string()),
                tags: None,
            }),
        },
        Node::MethodCall { method, args, .. } if matches!(method.as_str(), "render_prompt") => {
            if let Some(template_path) = args.first().and_then(literal_template_path) {
                if scan_stdlib_prompt_target(
                    &template_path,
                    &format!("harness.fs.{method}"),
                    args[0].span,
                    file_path,
                    source,
                    diagnostics,
                ) {
                    return;
                }
                if let Some(asset_ref) = harn_modules::asset_paths::parse(&template_path) {
                    let anchor = file_path.parent().unwrap_or(Path::new("."));
                    if let Err(err) = harn_modules::asset_paths::resolve(&asset_ref, anchor) {
                        // Surface resolver errors (no project root, unknown
                        // alias) before the file-existence check so the
                        // user sees the structural cause, not a generic
                        // "render target does not exist" message.
                        diagnostics.push(PreflightDiagnostic {
                            code: Code::ImportResolutionFailed,
                            path: file_path.display().to_string(),
                            source: source.to_string(),
                            span: args[0].span,
                            message: format!("preflight: {err}"),
                            help: Some(
                                "see docs/src/modules.md#package-root-prompt-assets for `@/...` and `@<alias>/...` syntax".to_string(),
                            ),
                            tags: None,
                        });
                        return;
                    }
                }
                let resolved = resolve_preflight_target(file_path, &template_path, config);
                if let Some(existing) = resolved
                    .iter()
                    .find(|path| super::result_cache::probe_exists(path))
                {
                    if let Ok(body) = super::result_cache::probe_read_to_string(existing) {
                        if let Err(err) = harn_prompt_syntax::validate_template_syntax(&body) {
                            diagnostics.push(PreflightDiagnostic {
                                code: Code::PromptTemplateParse,
                                path: file_path.display().to_string(),
                                source: source.to_string(),
                                span: args[0].span,
                                message: format!(
                                    "preflight: template '{template_path}' has a syntax error: {err}"
                                ),
                                help: Some(
                                    "see docs/src/prompt-templating.md for supported directives"
                                        .to_string(),
                                ),
                                tags: None,
                            });
                        }
                    }
                } else {
                    diagnostics.push(PreflightDiagnostic {
                        code: Code::PromptTargetMissing,
                        path: file_path.display().to_string(),
                        source: source.to_string(),
                        span: args[0].span,
                        message: format!(
                            "preflight: harness.fs.{method} target '{}' does not exist at {}",
                            template_path,
                            render_candidate_paths(&resolved)
                        ),
                        help: Some(render_target_miss_help(file_path, &template_path)),
                        tags: None,
                    });
                }
            }
        }
        Node::FunctionCall { name, args, .. } if name == "exec_at" || name == "shell_at" => {
            scan_execution_dir_preflight(args, file_path, source, diagnostics);
        }
        Node::FunctionCall { name, args, .. } if name == "spawn_agent" => {
            if let Some(agent_config) = args.last() {
                scan_spawn_agent_preflight(agent_config, file_path, source, diagnostics);
            }
        }
        Node::FunctionCall { name, .. } if name == "host_invoke" => {
            diagnostics.push(PreflightDiagnostic {
                code: Code::DeprecatedStdlibSymbol,
                path: file_path.display().to_string(),
                source: source.to_string(),
                span: node.span,
                message: "preflight: host_invoke(...) was removed; use host_call(\"capability.operation\", args)".to_string(),
                help: Some(
                    "replace host_invoke(\"project\", \"scan\", {}) with host_call(\"project.scan\", {})"
                        .to_string(),
                ),
                tags: None,
            });
        }
        Node::FunctionCall { name, args, .. } if name == "tool_define" => {
            // harn#743: when a tool declares `executor: "host_bridge"`,
            // its `host_capability` must point at a real host operation
            // — otherwise the model gets a tool whose dispatch will
            // fail at runtime with no static feedback today. Validate
            // the same capability map `host_call(...)` checks against
            // so the failure surfaces during `harn check`.
            scan_tool_define_preflight(
                node,
                args,
                host_capabilities,
                file_path,
                source,
                diagnostics,
            );
        }
        Node::FunctionCall { name, args, .. } if name == "host_call" => {
            if let Some((cap, op, params_arg)) = parse_host_call_args(args) {
                if !is_known_host_operation(host_capabilities, &cap, &op) {
                    diagnostics.push(PreflightDiagnostic {
                        code: Code::CapabilityUnknownOperation,
                        path: file_path.display().to_string(),
                        source: source.to_string(),
                        span: node.span,
                        message: format!(
                            "preflight: unknown host capability/operation '{cap}.{op}'"
                        ),
                        help: Some(
                            "declare additional host capabilities in [check].host_capabilities, [check].host_capabilities_path, --host-capabilities, or suppress via [check].preflight_allow"
                                .to_string(),
                        ),
                        tags: Some(format!("{cap}.{op}")),
                    });
                } else {
                    scan_host_param_discriminators(
                        node,
                        params_arg,
                        host_capabilities,
                        &cap,
                        &op,
                        file_path,
                        source,
                        diagnostics,
                    );
                }
                if cap == "template" && op == "render" {
                    if let Some(template_path) = host_render_path_arg(params_arg) {
                        if !scan_stdlib_prompt_target(
                            &template_path,
                            "host template render",
                            params_arg.map(|arg| arg.span).unwrap_or(node.span),
                            file_path,
                            source,
                            diagnostics,
                        ) {
                            if let Some(asset_ref) =
                                harn_modules::asset_paths::parse(&template_path)
                            {
                                let anchor = file_path.parent().unwrap_or(Path::new("."));
                                if let Err(err) =
                                    harn_modules::asset_paths::resolve(&asset_ref, anchor)
                                {
                                    diagnostics.push(PreflightDiagnostic {
                                        code: Code::ImportResolutionFailed,
                                        path: file_path.display().to_string(),
                                        source: source.to_string(),
                                        span: params_arg.map(|arg| arg.span).unwrap_or(node.span),
                                        message: format!("preflight: {err}"),
                                        help: Some(
                                            "see docs/src/modules.md#package-root-prompt-assets for `@/...` and `@<alias>/...` syntax".to_string(),
                                        ),
                                        tags: None,
                                    });
                                    return;
                                }
                            }
                            let resolved =
                                resolve_preflight_target(file_path, &template_path, config);
                            if !resolved
                                .iter()
                                .any(|path| super::result_cache::probe_exists(path))
                            {
                                diagnostics.push(PreflightDiagnostic {
                                    code: Code::PromptTargetMissing,
                                    path: file_path.display().to_string(),
                                    source: source.to_string(),
                                    span: params_arg.map(|arg| arg.span).unwrap_or(node.span),
                                    message: format!(
                                        "preflight: host template render target '{}' does not exist at {}",
                                        template_path,
                                        render_candidate_paths(&resolved)
                                    ),
                                    help: Some(
                                        "verify the template path, or set [check].bundle_root / --bundle-root when validating bundled layouts. Use `@/...` for project-root paths"
                                            .to_string(),
                                    ),
                                    tags: None,
                                });
                            }
                        }
                    }
                }
            } else if let Some(arg) = args.first() {
                diagnostics.push(PreflightDiagnostic {
                    code: Code::CapabilityCallStaticNameRequired,
                    path: file_path.display().to_string(),
                    source: source.to_string(),
                    span: arg.span,
                    message: "preflight: host_call(...) requires a literal \"capability.operation\" name for static validation".to_string(),
                    help: Some(
                        "use a string literal like host_call(\"project.scan\", {}) so preflight can validate the capability contract"
                            .to_string(),
                    ),
                    tags: None,
                });
            }
        }
        Node::MethodCall {
            object,
            method,
            args,
        }
        | Node::OptionalMethodCall {
            object,
            method,
            args,
        } if is_process_execution_method(object, method) => {
            scan_execution_dir_preflight(args, file_path, source, diagnostics);
        }
        _ => {}
    }
}
