pub(crate) mod agents_conformance;
pub(crate) mod bench;
pub(crate) mod check;
pub(crate) mod codemod;
pub(crate) mod config_cmd;
pub(crate) mod connect;
pub(crate) mod connector;
pub(crate) mod connector_schema_codegen;
pub(crate) mod contracts;
pub(crate) mod counterfactual;
pub(crate) mod crystallize;
pub mod demo;
pub(crate) mod dev;
pub(crate) mod diagnostics_catalog;
pub(crate) mod doctor;
pub(crate) mod dump_highlight_keywords;
pub(crate) mod dump_protocol_artifacts;
pub(crate) mod dump_trigger_quickref;
pub mod eval_coding_agent;
pub(crate) mod eval_coding_agent_preset;
pub mod eval_context;
pub(crate) mod eval_model_selector;
pub mod eval_prompt;
pub(crate) mod eval_prompt_context;
pub(crate) mod eval_scope_triage;
pub mod eval_skill_gate;
pub(crate) mod eval_tool_calls;
pub(crate) mod explain;
pub(crate) mod fix;
pub mod flow;
pub(crate) mod graph;
pub(crate) mod guard;
pub(crate) mod hardware;
pub(crate) mod init;
pub(crate) mod json_schemas;
pub(crate) mod local;
pub(crate) mod local_readiness;
pub(crate) mod mcp;
pub(crate) mod merge_captain;
pub(crate) mod merge_captain_mock;
pub(crate) mod models;
pub mod orchestrator;
pub mod pack;
pub(crate) mod package_scaffold;
pub(crate) mod parse_tokens;
pub mod persona;
pub mod persona_doctor;
pub mod persona_scaffold;
pub mod persona_supervision;
pub(crate) mod pg_codegen;
pub mod playground;
pub(crate) mod portal;
pub mod precompile;
pub(crate) mod protocol_conformance;
pub(crate) mod provider;
pub(crate) mod provider_capabilities;
pub(crate) mod provider_support;
pub(crate) mod providers;
pub(crate) mod quickstart;
pub(crate) mod repl;
pub(crate) mod replay;
pub(crate) mod routes;
pub(crate) mod rule;
pub(crate) mod rules_cli;
pub mod run;
pub(crate) mod scaffold_common;
pub(crate) mod scan;
pub(crate) mod serve;
pub(crate) mod session;
pub(crate) mod skill;
pub(crate) mod skills;
pub(crate) mod supervisor;
pub(crate) mod test;
pub mod test_bench;
pub mod time;
pub(crate) mod tool;
pub(crate) mod tool_mode_parity;
pub(crate) mod trace;
pub mod trigger;
pub(crate) mod trust;
pub(crate) mod try_cmd;
pub(crate) mod upgrade;
pub(crate) mod viz;
pub(crate) mod workflow;

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

/// Nearest-rank percentile over a pre-sorted slice. `quantile` is clamped to
/// `[0, 1]`; returns `None` for an empty slice. Shared by the latency
/// summaries in the eval and orchestrator stats commands.
pub(crate) fn nearest_rank_percentile(sorted: &[u64], quantile: f64) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((sorted.len() as f64 * quantile.clamp(0.0, 1.0)).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted.get(rank).copied()
}

const GENERATED_SOURCE_WALK_DIRS: &[&str] = &[
    ".burin",
    ".build",
    ".claude",
    ".codex",
    ".git",
    ".harn",
    ".harn-runs",
    ".next",
    ".svelte-kit",
    ".turbo",
    ".venv",
    "build",
    "coverage",
    "dist",
    "node_modules",
    "target",
];

pub(crate) fn should_skip_recursive_source_dir(dir: &Path) -> bool {
    let Some(name) = dir.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    GENERATED_SOURCE_WALK_DIRS.contains(&name) || name.starts_with(".harn-")
}

pub(crate) fn should_skip_recursive_source_file(file: &Path) -> bool {
    let Some(name) = file.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.starts_with(".harn-")
}

#[derive(Default)]
pub(crate) struct SourceTargets {
    pub(crate) harn: Vec<PathBuf>,
    pub(crate) prompts: Vec<PathBuf>,
}

impl SourceTargets {
    fn sort_and_dedup(&mut self) {
        self.harn.sort();
        self.harn.dedup();
        self.prompts.sort();
        self.prompts.dedup();
    }
}

pub(crate) fn collect_source_targets(
    targets: &[&str],
    include_harn: bool,
    include_prompts: bool,
) -> SourceTargets {
    let mut files = SourceTargets::default();
    for target in targets {
        let path = Path::new(target);
        if path.is_dir() {
            collect_source_targets_dir(path, include_harn, include_prompts, &mut files);
        } else {
            push_matching_source_target(path, include_harn, include_prompts, false, &mut files);
        }
    }
    files.sort_and_dedup();
    files
}

fn collect_source_targets_dir(
    dir: &Path,
    include_harn: bool,
    include_prompts: bool,
    files: &mut SourceTargets,
) {
    let root = dir.to_path_buf();
    let mut walker = WalkBuilder::new(dir);
    walker
        .hidden(false)
        .ignore(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .parents(true)
        .follow_links(false)
        .filter_entry(move |entry| {
            let path = entry.path();
            if path == root {
                return true;
            }
            if path.is_dir() {
                !should_skip_recursive_source_dir(path)
            } else {
                !should_skip_recursive_source_file(path)
            }
        });

    for entry in walker.build().filter_map(Result::ok) {
        let path = entry.path();
        if entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
        {
            push_matching_source_target(path, include_harn, include_prompts, true, files);
        }
    }
}

fn push_matching_source_target(
    path: &Path,
    include_harn: bool,
    include_prompts: bool,
    honor_skip_marker: bool,
    files: &mut SourceTargets,
) {
    if include_prompts && is_harn_prompt_file(path) {
        files.prompts.push(path.to_path_buf());
    } else if include_harn && is_harn_program_file(path) {
        let skip_marker = path.with_extension("conformance-skip");
        if !honor_skip_marker || !skip_marker.exists() {
            files.harn.push(path.to_path_buf());
        }
    }
}

pub(crate) fn is_harn_program_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if name.ends_with(".harn.prompt") || name.ends_with(".prompt") {
        return false;
    }
    name.ends_with(".harn") || name.ends_with(".harn.txt")
}

pub(crate) fn is_harn_prompt_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.ends_with(".harn.prompt") || name.ends_with(".prompt")
}

/// Recursively collect `.harn` files under `dir`, sorted by path. Files with a
/// sibling `<name>.conformance-skip` marker are excluded — used to temporarily
/// park tests that are tracking a known regression in an issue so `make test`
/// + `harn test conformance` can stay green while the fix is in flight.
pub(crate) fn collect_harn_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.path());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                if should_skip_recursive_source_dir(&path) {
                    continue;
                }
                collect_harn_files(&path, out);
            } else if should_skip_recursive_source_file(&path) {
                continue;
            } else if path.extension().is_some_and(|ext| ext == "harn") {
                let skip_marker = path.with_extension("conformance-skip");
                if skip_marker.exists() {
                    continue;
                }
                out.push(path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::nearest_rank_percentile;

    #[test]
    fn percentile_handles_empty_and_bounds() {
        assert_eq!(nearest_rank_percentile(&[], 0.5), None);
        let data = [10u64, 20, 30, 40, 50];
        assert_eq!(nearest_rank_percentile(&data, 0.0), Some(10));
        assert_eq!(nearest_rank_percentile(&data, 0.5), Some(30));
        assert_eq!(nearest_rank_percentile(&data, 1.0), Some(50));
        // Out-of-range quantiles clamp instead of panicking.
        assert_eq!(nearest_rank_percentile(&data, 2.0), Some(50));
        assert_eq!(nearest_rank_percentile(&[7u64], 0.99), Some(7));
    }
}
