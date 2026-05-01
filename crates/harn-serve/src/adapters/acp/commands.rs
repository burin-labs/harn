//! Slash-command discovery and invocation parsing for the ACP adapter.
//!
//! The ACP `available_commands_update` session notification advertises
//! agent-supported slash-commands to clients (e.g. Zed). Harn discovers
//! commands from top-level `pipeline` declarations annotated with
//! `@command(name?, description?, hint?)` in the loaded `.harn` source.
//!
//! Wire shape (canonical ACP, schema v0.12.2):
//! ```json
//! {
//!   "method": "session/update",
//!   "params": {
//!     "sessionId": "...",
//!     "update": {
//!       "sessionUpdate": "available_commands_update",
//!       "availableCommands": [
//!         { "name": "review", "description": "Review the pending diff",
//!           "input": { "hint": "(optional focus area)" } }
//!       ]
//!     }
//!   }
//! }
//! ```

use harn_parser::{parse_source, peel_attributes, Node};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DiscoveredCommand {
    /// Slash-command name advertised to the client (e.g. `"review"`).
    pub name: String,
    /// Pipeline decl name compiled when the command is invoked. Defaults
    /// to `name` when `@command(name: ...)` is omitted.
    pub pipeline_name: String,
    pub description: String,
    pub hint: Option<String>,
}

/// Parse `source` and collect top-level pipelines tagged `@command(...)`.
///
/// Sources that fail to parse return an empty list — the regular compile
/// path produces a diagnostic when the prompt runs, so this stays silent
/// to avoid double-reporting at discovery time.
///
/// Duplicate command names are de-duplicated by first-occurrence; a later
/// `@command(name: "foo")` after an earlier one is dropped silently. The
/// type checker doesn't flag duplicates because two pipelines tagged with
/// the same advertised name are still legal Harn — only the slash router
/// has to pick one.
pub(super) fn discover_commands(source: &str) -> Vec<DiscoveredCommand> {
    let Ok(program) = parse_source(source) else {
        return Vec::new();
    };
    let mut commands: Vec<DiscoveredCommand> = Vec::new();
    for sn in &program {
        let (attrs, inner) = peel_attributes(sn);
        let Node::Pipeline {
            name: pipeline_name,
            ..
        } = &inner.node
        else {
            continue;
        };
        let Some(attr) = attrs.iter().find(|a| a.name == "command") else {
            continue;
        };
        let cmd_name = attr
            .string_arg("name")
            .unwrap_or_else(|| pipeline_name.clone());
        if commands.iter().any(|c| c.name == cmd_name) {
            continue;
        }
        let description = attr.string_arg("description").unwrap_or_default();
        let hint = attr.string_arg("hint");
        commands.push(DiscoveredCommand {
            name: cmd_name,
            pipeline_name: pipeline_name.clone(),
            description,
            hint,
        });
    }
    commands
}

/// Render `commands` as the canonical ACP `availableCommands` JSON array.
pub(super) fn render_available_commands(commands: &[DiscoveredCommand]) -> serde_json::Value {
    let items: Vec<serde_json::Value> = commands
        .iter()
        .map(|cmd| {
            let mut entry = serde_json::json!({
                "name": cmd.name,
                "description": cmd.description,
            });
            if let Some(hint) = &cmd.hint {
                entry["input"] = serde_json::json!({ "hint": hint });
            }
            entry
        })
        .collect();
    serde_json::Value::Array(items)
}

/// Detect a `/<name> [args...]` slash invocation in the prompt text.
///
/// Returns `Some((name, args))` only when the prompt begins with a slash
/// followed by `[A-Za-z0-9_-]+`. Leading whitespace before the slash and
/// any whitespace (including newlines) between the name and args is
/// stripped — args is the text the user typed after the command, with
/// internal whitespace preserved. Returns `None` for plain prompts so
/// the default pipeline path keeps working unchanged.
pub(super) fn parse_slash_invocation(prompt_text: &str) -> Option<(&str, &str)> {
    let rest = prompt_text.trim_start().strip_prefix('/')?;
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    let (name, after) = rest.split_at(end);
    let args = after.trim_start();
    Some((name, args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_commands_finds_attributed_pipelines() {
        let source = r#"
            @command(name: "review", description: "Run a code review", hint: "focus area")
            pipeline review_branch(task) { println("review") }

            pipeline default(task) { println("default") }

            @command(description: "Plan the work")
            pipeline plan(task) { println("plan") }
        "#;
        let commands = discover_commands(source);
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].name, "review");
        assert_eq!(commands[0].pipeline_name, "review_branch");
        assert_eq!(commands[0].description, "Run a code review");
        assert_eq!(commands[0].hint.as_deref(), Some("focus area"));
        assert_eq!(commands[1].name, "plan");
        assert_eq!(commands[1].pipeline_name, "plan");
        assert_eq!(commands[1].description, "Plan the work");
        assert!(commands[1].hint.is_none());
    }

    #[test]
    fn discover_commands_skips_unparseable_source() {
        assert!(discover_commands("@command pipeline broken( {").is_empty());
    }

    #[test]
    fn discover_commands_dedupes_by_advertised_name() {
        let source = r#"
            @command(name: "foo")
            pipeline first(task) { 1 }

            @command(name: "foo")
            pipeline second(task) { 2 }
        "#;
        let commands = discover_commands(source);
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].pipeline_name, "first");
    }

    #[test]
    fn discover_commands_returns_empty_when_no_attribute_present() {
        let source = "pipeline main(task) { println(\"hi\") }";
        assert!(discover_commands(source).is_empty());
    }

    #[test]
    fn render_available_commands_matches_acp_wire_shape() {
        let commands = vec![
            DiscoveredCommand {
                name: "review".to_string(),
                pipeline_name: "review_branch".to_string(),
                description: "Review the diff".to_string(),
                hint: Some("focus area".to_string()),
            },
            DiscoveredCommand {
                name: "plan".to_string(),
                pipeline_name: "plan".to_string(),
                description: "Plan the work".to_string(),
                hint: None,
            },
        ];
        let json = render_available_commands(&commands);
        assert_eq!(
            json,
            serde_json::json!([
                {
                    "name": "review",
                    "description": "Review the diff",
                    "input": {"hint": "focus area"},
                },
                {
                    "name": "plan",
                    "description": "Plan the work",
                },
            ])
        );
    }

    #[test]
    fn parse_slash_invocation_extracts_name_and_args() {
        assert_eq!(
            parse_slash_invocation("/review src/lib.rs"),
            Some(("review", "src/lib.rs"))
        );
        assert_eq!(parse_slash_invocation("/plan"), Some(("plan", "")));
        assert_eq!(
            parse_slash_invocation("/plan-it now"),
            Some(("plan-it", "now"))
        );
        assert_eq!(
            parse_slash_invocation("/plan_it now"),
            Some(("plan_it", "now"))
        );
        assert_eq!(
            parse_slash_invocation("/plan\nstep two"),
            Some(("plan", "step two"))
        );
    }

    #[test]
    fn parse_slash_invocation_rejects_non_slash_prompts() {
        assert_eq!(parse_slash_invocation("review the diff"), None);
        assert_eq!(parse_slash_invocation(""), None);
        // Bare slash with no token name → not a command (e.g. someone typing markdown).
        assert_eq!(parse_slash_invocation("/"), None);
        assert_eq!(parse_slash_invocation("/ leading space"), None);
        // Slash followed by punctuation isn't a command identifier.
        assert_eq!(parse_slash_invocation("//comment"), None);
    }
}
