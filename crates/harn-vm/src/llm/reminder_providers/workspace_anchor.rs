//! Standing workspace facts use the canonical reminder lifecycle.

use super::*;
use crate::workspace_anchor::{WorkspaceAnchor, WorkspaceAnchorContext};

impl ReminderProvider for WorkspaceAnchorProvider {
    fn id(&self) -> &'static str {
        WORKSPACE_ANCHOR_ID
    }

    fn subscribes_to(&self) -> &'static [HookEvent] {
        WORKSPACE_ANCHOR_EVENTS
    }

    fn evaluate(&self, ctx: &ProviderContext) -> Option<ReminderSpec> {
        let body = match crate::agent_sessions::workspace_anchor_context(&ctx.session_id)? {
            WorkspaceAnchorContext::Current(anchor) => format_workspace_anchor_body(&anchor),
            WorkspaceAnchorContext::Cleared => crate::llm::helpers::render_stdlib_prompt_asset(
                "llm/prompts/workspace_anchor_cleared.harn.prompt",
                None,
            )
            .expect("workspace_anchor_cleared.harn.prompt is embedded and must render"),
        };
        let mut reminder = provider_reminder(body, WORKSPACE_ANCHOR_ID, ctx);
        reminder.ttl_turns = None;
        reminder.preserve_on_compact = true;
        Some(reminder)
    }
}

fn format_workspace_anchor_body(anchor: &WorkspaceAnchor) -> String {
    let mut body = format!(
        "<workspace-anchor>\nprimary: {}",
        anchor.primary.to_string_lossy()
    );
    if anchor.additional_roots.is_empty() {
        body.push_str("\nadditional_roots: []");
    } else {
        body.push_str("\nadditional_roots:");
        for root in &anchor.additional_roots {
            body.push_str("\n  - ");
            body.push_str(&root.path.to_string_lossy());
            body.push_str(" (mount_mode: ");
            body.push_str(root.mount_mode.as_str());
            body.push_str(", mounted_at: ");
            body.push_str(&root.mounted_at);
            body.push(')');
        }
    }
    body.push_str("\n</workspace-anchor>");
    body
}
