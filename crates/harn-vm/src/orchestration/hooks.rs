//! Tool lifecycle hooks — pre/post-execution interception.

use std::cell::RefCell;
use std::rc::Rc;

/// Action returned by a PreToolUse hook.
#[derive(Clone, Debug)]
pub enum PreToolAction {
    /// Allow the tool call to proceed unchanged.
    Allow,
    /// Deny the tool call with an explanation.
    Deny(String),
    /// Allow but replace the arguments.
    Modify(serde_json::Value),
}

/// Action returned by a PostToolUse hook.
#[derive(Clone, Debug)]
pub enum PostToolAction {
    /// Pass the result through unchanged.
    Pass,
    /// Replace the result text.
    Modify(String),
}

/// Callback types for tool lifecycle hooks.
pub type PreToolHookFn = Rc<dyn Fn(&str, &serde_json::Value) -> PreToolAction>;
pub type PostToolHookFn = Rc<dyn Fn(&str, &str) -> PostToolAction>;

/// A registered tool hook with a name pattern and callbacks.
#[derive(Clone)]
pub struct ToolHook {
    /// Glob-style pattern matched against tool names (e.g. `"*"`, `"exec*"`, `"read_file"`).
    pub pattern: String,
    /// Called before tool execution. Return `Deny` to reject, `Modify` to rewrite args.
    pub pre: Option<PreToolHookFn>,
    /// Called after tool execution with the result text. Return `Modify` to rewrite.
    pub post: Option<PostToolHookFn>,
}

impl std::fmt::Debug for ToolHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolHook")
            .field("pattern", &self.pattern)
            .field("has_pre", &self.pre.is_some())
            .field("has_post", &self.post.is_some())
            .finish()
    }
}

thread_local! {
    pub(super) static TOOL_HOOKS: RefCell<Vec<ToolHook>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn glob_match(pattern: &str, name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return name.starts_with(prefix);
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return name.ends_with(suffix);
    }
    pattern == name
}

pub fn register_tool_hook(hook: ToolHook) {
    TOOL_HOOKS.with(|hooks| hooks.borrow_mut().push(hook));
}

pub fn clear_tool_hooks() {
    TOOL_HOOKS.with(|hooks| hooks.borrow_mut().clear());
}

/// Run all matching PreToolUse hooks. Returns the final action.
pub fn run_pre_tool_hooks(tool_name: &str, args: &serde_json::Value) -> PreToolAction {
    TOOL_HOOKS.with(|hooks| {
        let hooks = hooks.borrow();
        let mut current_args = args.clone();
        for hook in hooks.iter() {
            if !glob_match(&hook.pattern, tool_name) {
                continue;
            }
            if let Some(ref pre) = hook.pre {
                match pre(tool_name, &current_args) {
                    PreToolAction::Allow => {}
                    PreToolAction::Deny(reason) => return PreToolAction::Deny(reason),
                    PreToolAction::Modify(new_args) => {
                        current_args = new_args;
                    }
                }
            }
        }
        if current_args != *args {
            PreToolAction::Modify(current_args)
        } else {
            PreToolAction::Allow
        }
    })
}

/// Run all matching PostToolUse hooks. Returns the (possibly modified) result.
pub fn run_post_tool_hooks(tool_name: &str, result: &str) -> String {
    TOOL_HOOKS.with(|hooks| {
        let hooks = hooks.borrow();
        let mut current = result.to_string();
        for hook in hooks.iter() {
            if !glob_match(&hook.pattern, tool_name) {
                continue;
            }
            if let Some(ref post) = hook.post {
                match post(tool_name, &current) {
                    PostToolAction::Pass => {}
                    PostToolAction::Modify(new_result) => {
                        current = new_result;
                    }
                }
            }
        }
        current
    })
}
