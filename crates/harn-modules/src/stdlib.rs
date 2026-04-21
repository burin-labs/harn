//! Embedded stdlib sources mirrored from `harn-vm` so the module graph can
//! resolve `import "std/<module>"` without taking a hard dependency on the
//! runtime crate.
//!
//! Keep the crate-local `src/stdlib/*.harn` copies in sync with
//! `harn-vm/src/stdlib*.harn`; `scripts/verify_crate_packages.sh` checks the
//! mirror and the packaged crate contents.

use std::path::PathBuf;

pub(crate) const STDLIB_SOURCES: &[(&str, &str)] = &[
    ("text", include_str!("stdlib/stdlib_text.harn")),
    (
        "collections",
        include_str!("stdlib/stdlib_collections.harn"),
    ),
    ("math", include_str!("stdlib/stdlib_math.harn")),
    ("path", include_str!("stdlib/stdlib_path.harn")),
    ("json", include_str!("stdlib/stdlib_json.harn")),
    ("schema", include_str!("stdlib/stdlib_schema.harn")),
    ("testing", include_str!("stdlib/stdlib_testing.harn")),
    ("vision", include_str!("stdlib/stdlib_vision.harn")),
    ("context", include_str!("stdlib/stdlib_context.harn")),
    ("runtime", include_str!("stdlib/stdlib_runtime.harn")),
    ("review", include_str!("stdlib/stdlib_review.harn")),
    (
        "experiments",
        include_str!("stdlib/stdlib_experiments.harn"),
    ),
    ("project", include_str!("stdlib/stdlib_project.harn")),
    ("async", include_str!("stdlib/stdlib_async.harn")),
    ("agents", include_str!("stdlib/stdlib_agents.harn")),
    (
        "agent_state",
        include_str!("stdlib/stdlib_agent_state.harn"),
    ),
    ("checkpoint", include_str!("stdlib/stdlib_checkpoint.harn")),
    ("host", include_str!("stdlib/stdlib_host.harn")),
    ("hitl", include_str!("stdlib/stdlib_hitl.harn")),
    ("waitpoints", include_str!("stdlib/stdlib_waitpoints.harn")),
    ("waitpoint", include_str!("stdlib/stdlib_waitpoint.harn")),
    ("monitors", include_str!("stdlib/stdlib_monitors.harn")),
    ("worktree", include_str!("stdlib/stdlib_worktree.harn")),
    ("acp", include_str!("stdlib/stdlib_acp.harn")),
    ("triggers", include_str!("stdlib/stdlib_triggers.harn")),
    (
        "connectors/github",
        include_str!("stdlib/stdlib_connectors_github.harn"),
    ),
    (
        "connectors/linear",
        include_str!("stdlib/stdlib_connectors_linear.harn"),
    ),
    (
        "connectors/notion",
        include_str!("stdlib/stdlib_connectors_notion.harn"),
    ),
    (
        "connectors/slack",
        include_str!("stdlib/stdlib_connectors_slack.harn"),
    ),
];

/// Return the embedded stdlib source for `module` (the part after
/// `std/`), or `None` if no stdlib module with that name exists.
pub(crate) fn get_stdlib_source(module: &str) -> Option<&'static str> {
    STDLIB_SOURCES
        .iter()
        .find_map(|(name, source)| (*name == module).then_some(*source))
}

/// Sentinel path used to key embedded stdlib modules in the module
/// graph. Real files never resolve to this path, so collisions are
/// impossible.
pub(crate) fn stdlib_virtual_path(module: &str) -> PathBuf {
    PathBuf::from(format!("<std>/{module}"))
}
