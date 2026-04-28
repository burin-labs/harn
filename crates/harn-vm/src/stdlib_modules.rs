/// Embedded standard library modules.
///
/// Each module is a `.harn` source file compiled into the binary via `include_str!`.
/// They are only parsed/executed when a script does `import "std/<module>"`.
pub fn get_stdlib_source(module: &str) -> Option<&'static str> {
    match module {
        "text" => Some(include_str!("stdlib_text.harn")),
        "collections" => Some(include_str!("stdlib_collections.harn")),
        "math" => Some(include_str!("stdlib_math.harn")),
        "path" => Some(include_str!("stdlib_path.harn")),
        "json" => Some(include_str!("stdlib_json.harn")),
        "graphql" => Some(include_str!("stdlib_graphql.harn")),
        "schema" => Some(include_str!("stdlib_schema.harn")),
        "testing" => Some(include_str!("stdlib_testing.harn")),
        "vision" => Some(include_str!("stdlib_vision.harn")),
        "context" => Some(include_str!("stdlib_context.harn")),
        "runtime" => Some(include_str!("stdlib_runtime.harn")),
        "review" => Some(include_str!("stdlib_review.harn")),
        "experiments" => Some(include_str!("stdlib_experiments.harn")),
        "project" => Some(include_str!("stdlib_project.harn")),
        "prompt_library" => Some(include_str!("stdlib_prompt_library.harn")),
        "async" => Some(include_str!("stdlib_async.harn")),
        "agents" => Some(include_str!("stdlib_agents.harn")),
        "agent_state" => Some(include_str!("stdlib_agent_state.harn")),
        "postgres" => Some(include_str!("stdlib_postgres.harn")),
        "checkpoint" => Some(include_str!("stdlib_checkpoint.harn")),
        "host" => Some(include_str!("stdlib_host.harn")),
        "hitl" => Some(include_str!("stdlib_hitl.harn")),
        "plan" => Some(include_str!("stdlib_plan.harn")),
        "waitpoints" => Some(include_str!("stdlib_waitpoints.harn")),
        "waitpoint" => Some(include_str!("stdlib_waitpoint.harn")),
        "monitors" => Some(include_str!("stdlib_monitors.harn")),
        "worktree" => Some(include_str!("stdlib_worktree.harn")),
        "acp" => Some(include_str!("stdlib_acp.harn")),
        "triggers" => Some(include_str!("stdlib_triggers.harn")),
        "connectors/github" => Some(include_str!("stdlib_connectors_github.harn")),
        "connectors/linear" => Some(include_str!("stdlib_connectors_linear.harn")),
        "connectors/notion" => Some(include_str!("stdlib_connectors_notion.harn")),
        "connectors/slack" => Some(include_str!("stdlib_connectors_slack.harn")),
        _ => None,
    }
}
