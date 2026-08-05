use crate::value::{VmError, VmValue};

use super::{
    collect_hook_effects_and_action, inject_hook_effects, wrap_post_tool_effects, HookEffect,
    HookEvent, ReminderSpec,
};

#[derive(Clone, Debug)]
pub enum PostToolAction {
    Pass,
    Modify(String),
    /// Replace the result and account for bytes removed from model-visible
    /// output. This survives a later hook appending text.
    Truncate {
        result: String,
        dropped_bytes: usize,
    },
    Reminder {
        spec: ReminderSpec,
        then: Box<PostToolAction>,
    },
}

/// Final PostToolUse output plus cumulative, hook-declared data loss.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PostToolHookResult {
    pub text: String,
    pub dropped_bytes: usize,
}

impl PostToolHookResult {
    pub(super) fn unchanged(text: &str) -> Self {
        Self {
            text: text.to_string(),
            dropped_bytes: 0,
        }
    }
}

pub(super) fn parse_post_tool_result(value: VmValue) -> Result<PostToolAction, VmError> {
    let (value, effects) =
        collect_hook_effects_and_action(HookEvent::PostToolUse, value, VmValue::Nil)?;
    match value {
        VmValue::Nil => Ok(wrap_post_tool_effects(effects, PostToolAction::Pass)),
        VmValue::String(text) => Ok(wrap_post_tool_effects(
            effects,
            PostToolAction::Modify(text.to_string()),
        )),
        VmValue::Dict(map) => {
            if let Some(result) = map.get("result") {
                let result = result.display();
                let truncated = matches!(map.get("truncated"), Some(VmValue::Bool(true)));
                if truncated {
                    let dropped_bytes = map
                        .get("dropped_bytes")
                        .and_then(VmValue::as_int)
                        .filter(|count| *count > 0)
                        .ok_or_else(|| {
                            VmError::Runtime(
                                "PostToolUse {truncated: true} requires positive integer \
                                 dropped_bytes"
                                    .to_string(),
                            )
                        })? as usize;
                    return Ok(wrap_post_tool_effects(
                        effects,
                        PostToolAction::Truncate {
                            result,
                            dropped_bytes,
                        },
                    ));
                }
                return Ok(wrap_post_tool_effects(
                    effects,
                    PostToolAction::Modify(result),
                ));
            }
            Ok(wrap_post_tool_effects(effects, PostToolAction::Pass))
        }
        other => Err(VmError::Runtime(format!(
            "PostToolUse hook must return nil, string, {{result}}, or \
             {{result, truncated: true, dropped_bytes}}, got {}",
            other.type_name()
        ))),
    }
}

pub(super) fn apply_post_tool_action(
    action: PostToolAction,
    mut current: PostToolHookResult,
) -> Result<PostToolHookResult, VmError> {
    match action {
        PostToolAction::Pass => Ok(current),
        PostToolAction::Modify(new_result) => {
            current.text = new_result;
            Ok(current)
        }
        PostToolAction::Truncate {
            result,
            dropped_bytes,
        } => {
            current.text = result;
            current.dropped_bytes = current.dropped_bytes.saturating_add(dropped_bytes);
            Ok(current)
        }
        PostToolAction::Reminder { spec, then } => {
            inject_hook_effects(
                "",
                vec![HookEffect::Reminder(spec)],
                Some(HookEvent::PostToolUse),
            )?;
            apply_post_tool_action(*then, current)
        }
    }
}
