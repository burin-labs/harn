//! Canonical embedded Harn standard library source catalog.
//!
//! This crate intentionally contains only static source strings so runtime and
//! static tooling crates can share the same stdlib modules without depending on
//! each other.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StdlibSource {
    pub module: &'static str,
    pub source: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StdlibPromptAsset {
    pub path: &'static str,
    pub source: &'static str,
}

pub const STDLIB_SOURCES: &[StdlibSource] = &[
    StdlibSource {
        module: "text",
        source: include_str!("stdlib/stdlib_text.harn"),
    },
    StdlibSource {
        module: "collections",
        source: include_str!("stdlib/stdlib_collections.harn"),
    },
    StdlibSource {
        module: "math",
        source: include_str!("stdlib/stdlib_math.harn"),
    },
    StdlibSource {
        module: "path",
        source: include_str!("stdlib/stdlib_path.harn"),
    },
    StdlibSource {
        module: "json",
        source: include_str!("stdlib/stdlib_json.harn"),
    },
    StdlibSource {
        module: "graphql",
        source: include_str!("stdlib/stdlib_graphql.harn"),
    },
    StdlibSource {
        module: "schema",
        source: include_str!("stdlib/stdlib_schema.harn"),
    },
    StdlibSource {
        module: "testing",
        source: include_str!("stdlib/stdlib_testing.harn"),
    },
    StdlibSource {
        module: "files",
        source: include_str!("stdlib/stdlib_files.harn"),
    },
    StdlibSource {
        module: "vision",
        source: include_str!("stdlib/stdlib_vision.harn"),
    },
    StdlibSource {
        module: "context",
        source: include_str!("stdlib/stdlib_context.harn"),
    },
    StdlibSource {
        module: "runtime",
        source: include_str!("stdlib/stdlib_runtime.harn"),
    },
    StdlibSource {
        module: "review",
        source: include_str!("stdlib/stdlib_review.harn"),
    },
    StdlibSource {
        module: "experiments",
        source: include_str!("stdlib/stdlib_experiments.harn"),
    },
    StdlibSource {
        module: "project",
        source: include_str!("stdlib/stdlib_project.harn"),
    },
    StdlibSource {
        module: "prompt_library",
        source: include_str!("stdlib/stdlib_prompt_library.harn"),
    },
    StdlibSource {
        module: "async",
        source: include_str!("stdlib/stdlib_async.harn"),
    },
    StdlibSource {
        module: "agents",
        source: include_str!("stdlib/stdlib_agents.harn"),
    },
    StdlibSource {
        module: "agent_state",
        source: include_str!("stdlib/stdlib_agent_state.harn"),
    },
    StdlibSource {
        module: "memory",
        source: include_str!("stdlib/stdlib_memory.harn"),
    },
    StdlibSource {
        module: "postgres",
        source: include_str!("stdlib/stdlib_postgres.harn"),
    },
    StdlibSource {
        module: "checkpoint",
        source: include_str!("stdlib/stdlib_checkpoint.harn"),
    },
    StdlibSource {
        module: "host",
        source: include_str!("stdlib/stdlib_host.harn"),
    },
    StdlibSource {
        module: "git",
        source: include_str!("stdlib/stdlib_git.harn"),
    },
    StdlibSource {
        module: "hitl",
        source: include_str!("stdlib/stdlib_hitl.harn"),
    },
    StdlibSource {
        module: "trust",
        source: include_str!("stdlib/stdlib_trust.harn"),
    },
    StdlibSource {
        module: "corrections",
        source: include_str!("stdlib/stdlib_corrections.harn"),
    },
    StdlibSource {
        module: "plan",
        source: include_str!("stdlib/stdlib_plan.harn"),
    },
    StdlibSource {
        module: "waitpoints",
        source: include_str!("stdlib/stdlib_waitpoints.harn"),
    },
    StdlibSource {
        module: "waitpoint",
        source: include_str!("stdlib/stdlib_waitpoint.harn"),
    },
    StdlibSource {
        module: "monitors",
        source: include_str!("stdlib/stdlib_monitors.harn"),
    },
    StdlibSource {
        module: "worktree",
        source: include_str!("stdlib/stdlib_worktree.harn"),
    },
    StdlibSource {
        module: "acp",
        source: include_str!("stdlib/stdlib_acp.harn"),
    },
    StdlibSource {
        module: "triggers",
        source: include_str!("stdlib/stdlib_triggers.harn"),
    },
    StdlibSource {
        module: "personas/prelude",
        source: include_str!("stdlib/stdlib_personas_prelude.harn"),
    },
    StdlibSource {
        module: "connectors/shared",
        source: include_str!("stdlib/stdlib_connectors_shared.harn"),
    },
    StdlibSource {
        module: "connectors/github",
        source: include_str!("stdlib/stdlib_connectors_github.harn"),
    },
    StdlibSource {
        module: "connectors/linear",
        source: include_str!("stdlib/stdlib_connectors_linear.harn"),
    },
    StdlibSource {
        module: "connectors/notion",
        source: include_str!("stdlib/stdlib_connectors_notion.harn"),
    },
    StdlibSource {
        module: "connectors/slack",
        source: include_str!("stdlib/stdlib_connectors_slack.harn"),
    },
];

pub const STDLIB_PROMPT_ASSETS: &[StdlibPromptAsset] = &[
    StdlibPromptAsset {
        path: "agent/prompts/tool_contract_text.harn.prompt",
        source: include_str!("stdlib/agent/prompts/tool_contract_text.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "agent/prompts/action_turn_nudge.harn.prompt",
        source: include_str!("stdlib/agent/prompts/action_turn_nudge.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "agent/prompts/completion_judge_default.harn.prompt",
        source: include_str!("stdlib/agent/prompts/completion_judge_default.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "workflow/prompts/stage.harn.prompt",
        source: include_str!("stdlib/workflow/prompts/stage.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "orchestration/prompts/compaction_summary.harn.prompt",
        source: include_str!("stdlib/orchestration/prompts/compaction_summary.harn.prompt"),
    },
];

pub fn get_stdlib_source(module: &str) -> Option<&'static str> {
    STDLIB_SOURCES
        .iter()
        .find_map(|entry| (entry.module == module).then_some(entry.source))
}

pub fn get_stdlib_prompt_asset(path: &str) -> Option<&'static str> {
    let path = path.strip_prefix("std/").unwrap_or(path);
    STDLIB_PROMPT_ASSETS
        .iter()
        .find_map(|entry| (entry.path == path).then_some(entry.source))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{get_stdlib_prompt_asset, get_stdlib_source, STDLIB_PROMPT_ASSETS, STDLIB_SOURCES};

    #[test]
    fn stdlib_sources_are_non_empty() {
        for entry in STDLIB_SOURCES {
            assert!(
                !entry.source.trim().is_empty(),
                "{} should have non-empty source",
                entry.module
            );
        }
    }

    #[test]
    fn stdlib_source_names_are_unique() {
        let mut names = BTreeSet::new();
        for entry in STDLIB_SOURCES {
            assert!(names.insert(entry.module), "duplicate {}", entry.module);
        }
    }

    #[test]
    fn stdlib_prompt_assets_are_non_empty() {
        for entry in STDLIB_PROMPT_ASSETS {
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
        for entry in STDLIB_PROMPT_ASSETS {
            assert!(paths.insert(entry.path), "duplicate {}", entry.path);
        }
    }

    #[test]
    fn key_stdlib_modules_resolve() {
        for module in [
            "context",
            "waitpoint",
            "personas/prelude",
            "connectors/shared",
            "connectors/github",
            "connectors/linear",
            "connectors/notion",
            "connectors/slack",
        ] {
            assert!(
                get_stdlib_source(module).is_some(),
                "std/{module} should resolve"
            );
        }
    }

    #[test]
    fn key_stdlib_prompt_assets_resolve() {
        for path in [
            "std/agent/prompts/tool_contract_text.harn.prompt",
            "std/agent/prompts/action_turn_nudge.harn.prompt",
            "std/agent/prompts/completion_judge_default.harn.prompt",
            "std/workflow/prompts/stage.harn.prompt",
            "std/orchestration/prompts/compaction_summary.harn.prompt",
        ] {
            assert!(
                get_stdlib_prompt_asset(path).is_some(),
                "{path} should resolve"
            );
        }
    }
}
