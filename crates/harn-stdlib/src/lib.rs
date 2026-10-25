//! Canonical embedded Harn standard library source catalog.
//!
//! This crate intentionally contains only static source strings. The compiler,
//! runtime, and tooling read them through `harn-stdlib-catalog` and never
//! depend on this crate, so editing a stdlib module rebuilds only this crate
//! and the binaries that link it. A host calls [`install`] once at startup.
//! Loading this catalog is pure: modules cannot register ambient
//! effects, and every effectful stdlib function must receive its typed Harness
//! capability explicitly from the caller, including callbacks that may perform
//! effects.
//!
//! The sources are embedded as one zstd archive, about a fifth of their size,
//! which the first catalog read decodes for the rest of the process.

use std::sync::LazyLock;

pub use harn_stdlib_catalog::{
    builtin_reexports, StdlibCatalog, StdlibCliScript, StdlibPromptAsset, StdlibSource,
};

macro_rules! catalog_entries {
    (
        sources: [$($module:literal => $_module_path:literal),* $(,)?],
        prompt_assets: [$($asset:literal => $_asset_path:literal),* $(,)?],
        cli_scripts: [$($script:literal => $_script_path:literal),* $(,)?] $(,)?
    ) => {
        const MODULES: &[&str] = &[$($module),*];
        const PROMPT_ASSETS: &[&str] = &[$($asset),*];
        const CLI_SCRIPTS: &[&str] = &[$($script),*];
    };
}

include!("catalog_entries.rs");
include!(concat!(env!("OUT_DIR"), "/stdlib_archive.rs"));

/// Decode the archive and cut it into the catalog's entries, in the order
/// `build.rs` wrote them. The text lives for the rest of the process.
fn unpack() -> StdlibCatalog {
    let bytes = zstd::bulk::decompress(ARCHIVE, ARCHIVE_TEXT_LEN)
        .unwrap_or_else(|error| panic!("the embedded stdlib archive does not decode: {error}"));
    let mut rest: &'static str = String::from_utf8(bytes)
        .expect("build.rs packed UTF-8 text")
        .leak();
    let mut lengths = LENGTHS.iter();
    let mut next = || {
        let (source, tail) = rest.split_at(*lengths.next().expect("one length per entry"));
        rest = tail;
        source
    };
    let sources = MODULES
        .iter()
        .map(|&module| StdlibSource {
            module,
            source: next(),
        })
        .collect::<Vec<_>>();
    let prompt_assets = PROMPT_ASSETS
        .iter()
        .map(|&path| StdlibPromptAsset {
            path,
            source: next(),
        })
        .collect::<Vec<_>>();
    let cli_scripts = CLI_SCRIPTS
        .iter()
        .map(|&name| StdlibCliScript {
            name,
            source: next(),
        })
        .collect::<Vec<_>>();
    assert!(
        rest.is_empty() && lengths.next().is_none(),
        "the embedded stdlib archive does not match its catalog"
    );
    StdlibCatalog {
        sources: sources.leak(),
        prompt_assets: prompt_assets.leak(),
        cli_scripts: cli_scripts.leak(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdlibPublicFunction {
    pub name: String,
    pub signature: String,
    pub required_params: usize,
    pub total_params: usize,
    pub variadic: bool,
    pub doc: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdlibEntrypointModule {
    pub import_path: String,
    pub category: String,
}

/// Canonical normalized connector event schemas, authored as Harn `type`
/// declarations. This is the SOURCE OF TRUTH for the Rust event-payload
/// structs in `crates/harn-vm/src/triggers/event/schemas_generated.rs`, which
/// are generated from these declarations by `harn connector-schema-codegen`.
///
/// It is intentionally NOT registered in [`CATALOG`]: it is a codegen
/// input (like `crates/harn-serve/openapi.yaml`), not a module loaded into every
/// program.
/// Exposing it as an embedded string keeps the generator independent of the
/// current working directory.
pub const CONNECTOR_EVENT_SCHEMAS_SOURCE: &str = include_str!("stdlib/stdlib_event_schemas.harn");

/// This crate's catalog: every embedded stdlib module, prompt asset, and CLI
/// script, in the order `catalog_entries.rs` lists them.
pub static CATALOG: LazyLock<StdlibCatalog> = LazyLock::new(unpack);

/// Install [`CATALOG`] as the process's standard library. Hosts call this once
/// at startup, before running Harn code; calling it again is a no-op.
///
/// # Errors
///
/// Refuses when a different catalog is already installed.
pub fn install() -> Result<(), harn_stdlib_catalog::CatalogAlreadyInstalled> {
    harn_stdlib_catalog::install(&CATALOG)
}

pub fn get_stdlib_source(module: &str) -> Option<&'static str> {
    CATALOG.source(module)
}

/// Find an embedded CLI subcommand script by name. Returns the embedded
/// source string when present, or `None` if no script with that name is
/// registered in [`CATALOG`].
pub fn find_cli_script(name: &str) -> Option<&'static str> {
    CATALOG.cli_script(name)
}

pub fn get_stdlib_prompt_asset(path: &str) -> Option<&'static str> {
    CATALOG.prompt_asset(path)
}

pub fn public_functions_for_module(module: &str) -> Vec<StdlibPublicFunction> {
    let Some(source) = get_stdlib_source(module) else {
        return Vec::new();
    };
    public_functions_from_source(source)
}

pub fn entrypoint_modules() -> Vec<StdlibEntrypointModule> {
    CATALOG
        .sources
        .iter()
        .filter_map(|entry| {
            entrypoint_category_from_source(entry.source).map(|category| StdlibEntrypointModule {
                import_path: format!("std/{}", entry.module),
                category,
            })
        })
        .collect()
}

fn entrypoint_category_from_source(source: &str) -> Option<String> {
    for line in source.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(category) = line.strip_prefix("// @harn-entrypoint-category ") {
            let category = category.trim();
            return (!category.is_empty()).then(|| category.to_string());
        }
        if !line.starts_with("//") {
            return None;
        }
    }
    None
}

fn public_functions_from_source(source: &str) -> Vec<StdlibPublicFunction> {
    let mut out = Vec::new();
    let mut doc: Option<String> = None;
    let lines = source.lines().collect::<Vec<_>>();
    let mut index = 0usize;
    while index < lines.len() {
        let line = lines[index].trim();
        if line.starts_with("/**") {
            let (parsed, next) = parse_harndoc(&lines, index);
            doc = parsed;
            index = next;
            continue;
        }
        if line.starts_with("pub fn ") {
            let (signature_line, next) = collect_public_function_signature(&lines, index);
            if let Some(function) = parse_public_function_line(&signature_line, doc.take()) {
                out.push(function);
                index = next;
                continue;
            }
        }
        if let Some(function) = parse_public_function_line(line, doc.take()) {
            out.push(function);
        } else if !line.is_empty() && !line.starts_with("//") {
            doc = None;
        }
        index += 1;
    }
    out
}

fn collect_public_function_signature(lines: &[&str], start: usize) -> (String, usize) {
    let mut parts = Vec::new();
    let mut index = start;
    while index < lines.len() {
        parts.push(lines[index].trim().to_string());
        let candidate = parts.join(" ");
        if public_function_signature_complete(&candidate) {
            return (candidate, index + 1);
        }
        index += 1;
    }
    (parts.join(" "), index)
}

fn public_function_signature_complete(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("pub fn ") else {
        return false;
    };
    let Some((_, after_paren)) = rest.split_once('(') else {
        return false;
    };
    matching_paren_len(after_paren).is_some()
}

fn parse_harndoc(lines: &[&str], start: usize) -> (Option<String>, usize) {
    let mut parts = Vec::new();
    let mut index = start;
    while index < lines.len() {
        let mut line = lines[index].trim();
        if index == start {
            line = line.trim_start_matches("/**").trim();
        }
        let done = line.ends_with("*/");
        line = line.trim_end_matches("*/").trim();
        line = line.trim_start_matches('*').trim();
        if !line.is_empty() {
            parts.push(line.to_string());
        }
        index += 1;
        if done {
            break;
        }
    }
    let text = parts.join("\n").trim().to_string();
    ((!text.is_empty()).then_some(text), index)
}

#[expect(
    clippy::string_slice,
    reason = "bounds come from str::find and char_indices"
)]
fn parse_public_function_line(line: &str, doc: Option<String>) -> Option<StdlibPublicFunction> {
    let rest = line.strip_prefix("pub fn ")?.trim();
    let name_end = rest.find('(')?;
    let declaration_name = rest[..name_end].trim();
    let name = declaration_name
        .split_once('<')
        .map_or(declaration_name, |(name, _)| name.trim());
    if name.is_empty() {
        return None;
    }
    let params_start = name_end + 1;
    let params_len = matching_paren_len(&rest[params_start..])?;
    let params = &rest[params_start..params_start + params_len];
    let after = rest[params_start + params_len + 1..].trim();
    let return_type = after
        .strip_prefix("->")
        .and_then(|tail| tail.split('{').next())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let signature = match return_type {
        Some(ret) => format!("{declaration_name}({params}) -> {ret}"),
        None => format!("{declaration_name}({params})"),
    };
    let param_parts = split_top_level_params(params);
    let total_params = param_parts
        .iter()
        .filter(|param| !param.trim().is_empty())
        .count();
    let variadic = param_parts
        .iter()
        .any(|param| param.trim_start().starts_with("..."));
    let required_params = param_parts
        .iter()
        .filter(|param| {
            let param = param.trim();
            !param.is_empty() && !param.contains('=') && !param.starts_with("...")
        })
        .count();
    Some(StdlibPublicFunction {
        name: name.to_string(),
        signature,
        required_params,
        total_params,
        variadic,
        doc,
    })
}

fn matching_paren_len(input: &str) -> Option<usize> {
    let mut depth = 1usize;
    for (offset, ch) in input.char_indices() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(offset);
                }
            }
            _ => {}
        }
    }
    None
}

#[expect(
    clippy::string_slice,
    reason = "offsets come from char_indices and 1-byte ','"
)]
fn split_top_level_params(params: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0isize;
    let mut start = 0usize;
    for (offset, ch) in params.char_indices() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&params[start..offset]);
                start = offset + 1;
            }
            _ => {}
        }
    }
    out.push(&params[start..]);
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{
        entrypoint_modules, get_stdlib_prompt_asset, matching_paren_len,
        parse_public_function_line, public_functions_for_module, CATALOG,
    };

    #[test]
    fn stdlib_sources_are_non_empty() {
        for entry in CATALOG.sources {
            assert!(
                !entry.source.trim().is_empty(),
                "{} should have non-empty source",
                entry.module
            );
        }
    }

    #[test]
    fn cli_scripts_are_non_empty_and_uniquely_named() {
        let mut seen = BTreeSet::new();
        for entry in CATALOG.cli_scripts {
            assert!(
                !entry.source.trim().is_empty(),
                "cli/{} should have non-empty source",
                entry.name
            );
            assert!(
                seen.insert(entry.name),
                "cli/{} is registered more than once in CATALOG.cli_scripts",
                entry.name
            );
        }
    }

    #[test]
    fn stdlib_source_names_are_unique() {
        let mut names = BTreeSet::new();
        for entry in CATALOG.sources {
            assert!(names.insert(entry.module), "duplicate {}", entry.module);
        }
    }

    #[test]
    fn stdlib_prompt_assets_are_non_empty() {
        for entry in CATALOG.prompt_assets {
            assert!(
                !entry.source.trim().is_empty(),
                "{} should have non-empty prompt asset source",
                entry.path
            );
        }
    }

    #[test]
    fn stdlib_prompt_asset_paths_are_unique() {
        let mut paths = BTreeSet::new();
        for entry in CATALOG.prompt_assets {
            assert!(paths.insert(entry.path), "duplicate {}", entry.path);
        }
    }

    #[test]
    fn key_stdlib_prompt_assets_resolve() {
        for path in [
            "std/agent/prompts/tool_contract_text.harn.prompt",
            "std/agent/prompts/default_nudge.harn.prompt",
            "std/agent/prompts/completion_judge_default.harn.prompt",
            "std/llm/prompts/directive_envelope_instructions.harn.prompt",
            "std/llm/prompts/directive_nonce_authority.harn.prompt",
            "std/workflow/prompts/stage.harn.prompt",
            "std/orchestration/prompts/compaction_summary.harn.prompt",
            "std/orchestration/prompts/compaction_policy_extension.harn.prompt",
            "std/orchestration/prompts/compaction_policy_replacement.harn.prompt",
            "std/orchestration/prompts/compaction_state_grounding.harn.prompt",
        ] {
            assert!(
                get_stdlib_prompt_asset(path).is_some(),
                "{path} should resolve"
            );
        }
    }

    #[test]
    fn public_function_catalog_derives_signatures_from_harn_source() {
        let exports = public_functions_for_module("workflow/execute");
        assert_eq!(exports.len(), 1);
        assert_eq!(exports[0].name, "workflow_execute");
        assert_eq!(
            exports[0].signature,
            "workflow_execute( harness: Harness, task: string, graph: dict, artifacts: list<unknown>? = nil, options: dict? = nil, )"
        );
        assert_eq!(exports[0].required_params, 3);
        assert_eq!(exports[0].total_params, 5);
    }

    #[test]
    fn tool_registry_from_catalog_includes_cli_metadata_parameter() {
        let function = public_functions_for_module("tools")
            .into_iter()
            .find(|function| function.name == "tool_registry_from")
            .expect("std/tools should export tool_registry_from");
        assert_eq!(function.required_params, 1);
        assert_eq!(function.total_params, 2);
        assert!(function
            .signature
            .contains("options: ToolRegistryOptions? = nil"));
    }

    #[test]
    fn soft_landing_modules_own_their_public_surface() {
        // The cut-rule layer is three owners: the meter registry, the
        // predicate evaluator, and the latched landing machine. A rename that
        // drops one of these from its module is the failure this catches.
        for (module, expected) in [
            (
                "agent/run_meter",
                &[
                    "meter_observation_lower",
                    "meter_observation_render",
                    "meter_observation_upper",
                    "run_meter_bounded",
                    "run_meter_charge",
                    "run_meter_digest",
                    "run_meter_exact",
                    "run_meter_field_registered",
                    "run_meter_field_spec",
                    "run_meter_fields",
                    "run_meter_new",
                    "run_meter_observe",
                    "run_meter_project_next_call",
                    "run_meter_read",
                    "run_meter_registry_digest",
                    "run_meter_unavailable",
                ][..],
            ),
            (
                "agent/cut_landing",
                &[
                    "cut_action_allowed",
                    "cut_predicate_evaluate",
                    "cut_predicate_render",
                    "cut_predicate_validate",
                    "cut_rules_digest",
                    "cut_rules_tick",
                    "cut_rules_validate",
                    "cut_state_complete",
                    "cut_state_new",
                    "cut_state_record_admission",
                    "cut_state_record_terminal_emit",
                    "cut_terminal_emit_due",
                    "cut_terminal_evidence",
                ][..],
            ),
        ] {
            let exports = public_functions_for_module(module)
                .into_iter()
                .map(|function| function.name)
                .collect::<BTreeSet<_>>();
            for name in expected {
                assert!(exports.contains(*name), "std/{module} should export {name}");
            }
        }
    }

    #[test]
    fn pace_cut_rule_module_owns_public_functions() {
        let cut_rule_exports = public_functions_for_module("agent/cut_rules")
            .into_iter()
            .map(|function| function.name)
            .collect::<BTreeSet<_>>();
        for name in [
            "pace_cut_rule_action_of",
            "pace_cut_rule_check_max_injections",
            "pace_cut_rule_decision",
            "pace_cut_rule_extend_max",
        ] {
            assert!(
                cut_rule_exports.contains(name),
                "std/agent/cut_rules should export {name}"
            );
        }

        let governor_exports = public_functions_for_module("agent/governors")
            .into_iter()
            .map(|function| function.name)
            .collect::<BTreeSet<_>>();
        for removed in [
            "governor_pace_check_max_injections",
            "governor_pace_decision",
            "governor_pace_extend_max",
            "pace_action_of",
        ] {
            assert!(
                !governor_exports.contains(removed),
                "std/agent/governors must not retain removed export {removed}"
            );
        }
    }

    #[test]
    fn command_stdlib_module_exports_step_helpers() {
        let exports = public_functions_for_module("command")
            .into_iter()
            .map(|function| function.name)
            .collect::<BTreeSet<_>>();
        for name in [
            "command_run",
            "command_wait",
            "command_wait_for_output",
            "command_cancel",
            "command_run_streaming",
            "command_output_tail",
            "command_json",
            "command_json_step",
            "command_try",
            "command_step",
            "command_steps_append",
            "command_last_failed_step",
            "command_step_ref",
            "command_output_text",
            "argv_label",
            "shell_command_from_argv",
            "shell_command_from_value",
        ] {
            assert!(exports.contains(name), "std/command should export {name}");
        }
    }

    #[test]
    fn disclosure_stdlib_module_exports_render_helpers() {
        let exports = public_functions_for_module("disclosure")
            .into_iter()
            .map(|function| function.name)
            .collect::<BTreeSet<_>>();
        assert!(
            exports.contains("render"),
            "std/disclosure should export render"
        );
        assert!(
            exports.contains("git_trailers"),
            "std/disclosure should export git_trailers"
        );
        assert!(
            exports.contains("slack_message_disclosure"),
            "std/disclosure should export slack_message_disclosure"
        );
        assert!(
            exports.contains("append_git_trailers"),
            "std/disclosure should export append_git_trailers"
        );
    }

    #[test]
    fn async_stdlib_exports_predicate_backoff_name_only() {
        let exports = public_functions_for_module("async")
            .into_iter()
            .map(|function| function.name)
            .collect::<BTreeSet<_>>();
        assert!(
            exports.contains("retry_predicate_with_backoff"),
            "std/async should export retry_predicate_with_backoff"
        );
        assert!(
            !exports.contains("retry_with_backoff"),
            "std/async should not retain the old retry_with_backoff export"
        );
    }

    #[test]
    fn signal_stdlib_module_exports_interrupt_helpers() {
        let exports = public_functions_for_module("signal")
            .into_iter()
            .map(|function| function.name)
            .collect::<BTreeSet<_>>();
        for name in [
            "on_interrupt",
            "off_interrupt",
            "interrupted",
            "with_interrupt",
        ] {
            assert!(exports.contains(name), "std/signal should export {name}");
        }
    }

    #[test]
    fn git_stdlib_module_exports_local_wrappers() {
        let exports = public_functions_for_module("git")
            .into_iter()
            .map(|function| function.name)
            .collect::<BTreeSet<_>>();
        for name in [
            "git_run",
            "git_status",
            "git_current_branch",
            "git_log",
            "git_switch",
            "git_pull_ff_only",
            "git_find_tool",
            "git_run_tool",
            "git_tools",
            "git_toolbox_tools",
        ] {
            assert!(exports.contains(name), "std/git should export {name}");
        }
    }

    #[test]
    fn agent_workers_exports_suspend_resume_wrappers() {
        let exports = public_functions_for_module("agent/workers");
        let suspend = exports
            .iter()
            .find(|function| function.name == "suspend_agent")
            .expect("std/agent/workers should export suspend_agent");
        assert_eq!(
            suspend.signature,
            "suspend_agent( agents: HarnessAgent, worker: unknown, reason: string = \"\", options: dict? = nil, ) -> unknown"
        );
        assert_eq!(suspend.required_params, 2);
        assert_eq!(suspend.total_params, 4);

        let resume = exports
            .iter()
            .find(|function| function.name == "resume_agent")
            .expect("std/agent/workers should export resume_agent");
        assert_eq!(
            resume.signature,
            "resume_agent( agents: HarnessAgent, worker_or_snapshot: unknown, resume_input: unknown = nil, continue_transcript: bool = true, ) -> unknown"
        );
        assert_eq!(resume.required_params, 2);
        assert_eq!(resume.total_params, 4);

        let stop = exports
            .iter()
            .find(|function| function.name == "agent_stop")
            .expect("std/agent/workers should export agent_stop");
        assert_eq!(
            stop.signature,
            "agent_stop( agents: HarnessAgent, worker: unknown, options: AgentStopOptions? = nil, ) -> unknown"
        );
        assert_eq!(stop.required_params, 2);
        assert_eq!(stop.total_params, 3);

        let parse_resume = exports
            .iter()
            .find(|function| function.name == "parse_resume_conditions")
            .expect("std/agent/workers should export parse_resume_conditions");
        assert_eq!(
            parse_resume.signature,
            "parse_resume_conditions( agents: HarnessAgent, conditions: ResumeConditions? = nil, ) -> ResumeConditions?"
        );
        assert_eq!(parse_resume.required_params, 1);
        assert_eq!(parse_resume.total_params, 2);

        let lifecycle = exports
            .iter()
            .find(|function| function.name == "agent_lifecycle_tools")
            .expect("std/agent/workers should export agent_lifecycle_tools");
        assert_eq!(
            lifecycle.signature,
            "agent_lifecycle_tools( agents: HarnessAgent, registry: ToolRegistry? = nil, options: AgentLifecycleToolsOptions? = nil, ) -> ToolRegistry"
        );
        assert_eq!(lifecycle.required_params, 1);
        assert_eq!(lifecycle.total_params, 3);
    }

    #[test]
    fn tui_stdlib_module_exports_terminal_helpers() {
        let exports = public_functions_for_module("tui")
            .into_iter()
            .map(|function| function.name)
            .collect::<BTreeSet<_>>();
        for name in ["page", "terminal_width", "rule", "clear", "select_from"] {
            assert!(exports.contains(name), "std/tui should export {name}");
        }
    }

    #[test]
    fn semver_stdlib_module_exports_release_helpers() {
        let exports = public_functions_for_module("semver")
            .into_iter()
            .map(|function| function.name)
            .collect::<BTreeSet<_>>();
        for name in [
            "strip_v",
            "add_v",
            "is_v_semver",
            "is_v_release_semver",
            "parse",
            "parse_release",
            "is_prerelease_identifier",
            "is_prerelease",
            "compare_release",
            "max_canonical_tag",
            "next",
            "bump_type",
            "version_from_release_branch",
            "version_from_tag",
        ] {
            assert!(exports.contains(name), "std/semver should export {name}");
        }
    }

    #[test]
    fn changelog_stdlib_module_exports_typed_primitives() {
        let exports = public_functions_for_module("changelog")
            .into_iter()
            .map(|function| function.name)
            .collect::<BTreeSet<_>>();
        for name in [
            "changelog_validate_categories",
            "changelog_parse_fragment",
            "changelog_order_fragments",
            "changelog_normalize_fragment_body",
            "changelog_assemble_fragments",
            "changelog_parse_sections",
            "changelog_find_section",
            "changelog_merge_unreleased",
        ] {
            assert!(exports.contains(name), "std/changelog should export {name}");
        }
    }

    #[test]
    fn text_stdlib_module_exports_regex_and_pad_helpers() {
        let exports = public_functions_for_module("text")
            .into_iter()
            .map(|function| function.name)
            .collect::<BTreeSet<_>>();
        for name in [
            "pad_left",
            "pad_right",
            "repeat_string",
            "regex_first_capture",
            "regex_capture_groups",
            "regex_all_first_captures",
        ] {
            assert!(exports.contains(name), "std/text should export {name}");
        }
    }

    #[test]
    fn harn_entrypoint_catalog_is_declared_by_stdlib_sources() {
        let modules = entrypoint_modules();
        let entries = modules
            .iter()
            .map(|module| (module.import_path.as_str(), module.category.as_str()))
            .collect::<BTreeSet<_>>();
        for entry in [
            ("std/agent/loop", "agent.stdlib"),
            ("std/agent/primitives", "agent.stdlib"),
            ("std/workflow/execute", "workflow.stdlib"),
        ] {
            assert!(entries.contains(&entry), "{entry:?} should be declared");
        }
    }

    #[test]
    fn matching_paren_len_closes_on_every_bracket_kind() {
        // Each closer kind must terminate the scan once depth returns to zero,
        // mirroring the symmetric opener arm and `split_top_level_params`.
        assert_eq!(matching_paren_len("a, b)"), Some(4));
        assert_eq!(matching_paren_len("x: [int])"), Some(8));
        assert_eq!(matching_paren_len("x: {a: int})"), Some(11));
        // A top-level `]`/`}` is the matching close for the consumed opener and
        // must return rather than scanning to the end and yielding `None`.
        assert_eq!(matching_paren_len("x]"), Some(1));
        assert_eq!(matching_paren_len("x}"), Some(1));
        assert_eq!(matching_paren_len("unterminated"), None);
    }

    #[test]
    fn parse_public_function_line_handles_record_typed_params() {
        let parsed =
            parse_public_function_line("pub fn configure(opts: {retries: int}) -> bool", None)
                .expect("signature with a record-typed parameter should parse");
        assert_eq!(parsed.name, "configure");
        assert_eq!(parsed.signature, "configure(opts: {retries: int}) -> bool");
        assert_eq!(parsed.total_params, 1);
        assert_eq!(parsed.required_params, 1);
    }

    #[test]
    fn parse_public_function_line_indexes_generic_functions_by_base_name() {
        let parsed = parse_public_function_line(
            "pub fn map<T, U>(items: list<T>, project: fn(T) -> U) -> list<U>",
            None,
        )
        .expect("generic public function should parse");
        assert_eq!(parsed.name, "map");
        assert_eq!(
            parsed.signature,
            "map<T, U>(items: list<T>, project: fn(T) -> U) -> list<U>"
        );
        assert_eq!(parsed.total_params, 2);
        assert_eq!(parsed.required_params, 2);
    }
}
