//! Neutral computer-use tool projection.
//!
//! harn-vm owns the *semantic* layer of computer use. It projects the single
//! neutral `computer` function tool onto each provider's native computer-use
//! surface.
//!
//! The coordinate-native *execution* nucleus (screenshot capture, pointer /
//! keyboard input, accessibility tree) lives in `harn-hostlib`'s `computer`
//! module and is reached through the `hostlib_computer_*` builtins. Nothing in
//! this module touches the OS — it is pure projection so it stays
//! deterministic and unit-testable.
//!
//! ## De-overfitting
//!
//! There is exactly one host-facing `computer` tool. Per provider, this module
//! lowers it to:
//! - `native_anthropic` → the Anthropic `computer_20251124` tool,
//! - `native_anthropic_toolset` → `computer_toolset_20260801`, whose members
//!   retain the `computer` namespace through dispatch and result replay,
//! - `native_openai` → the OpenAI Responses `computer` tool,
//! - `function` / `grounded` / unset → left as the plain function-schema tool
//!   (the generic fallback), untouched.

use serde_json::{json, Value};

use crate::llm::capabilities::{Capabilities, ComputerUseStyle};

/// The host-facing neutral tool name every provider projection keys on.
pub(crate) const COMPUTER_TOOL_NAME: &str = "computer";

/// Default projected display width (Anthropic XGA). Used until the orchestrator
/// threads the real captured display size into the projection.
pub(crate) const DEFAULT_DISPLAY_WIDTH: u32 = 1024;
/// Default projected display height (Anthropic XGA).
pub(crate) const DEFAULT_DISPLAY_HEIGHT: u32 = 768;

/// The `name` a tool advertises, checking the top-level `name` first and then
/// the OpenAI `function.name` nesting.
fn tool_name(tool: &Value) -> Option<&str> {
    tool.get("name")
        .or_else(|| tool.get("function").and_then(|f| f.get("name")))
        .and_then(Value::as_str)
}

/// Whether `tool` is the plain function-schema `computer` tool (i.e. a tool the
/// host declared, not an already-projected provider-native computer tool).
pub(crate) fn is_computer_function_tool(tool: &Value) -> bool {
    let ty = tool.get("type").and_then(Value::as_str);
    // An already-projected native tool has a `computer*` type; skip it so the
    // projection is idempotent.
    if ty.is_some_and(|ty| ty.starts_with("computer")) {
        return false;
    }
    tool_name(tool) == Some(COMPUTER_TOOL_NAME)
}

/// The OpenAI Responses `environment` for the host OS. OpenAI accepts
/// `mac` / `windows` / `ubuntu` / `browser`; map the local platform onto the
/// desktop set, defaulting non-mac/-windows Unix to `ubuntu`.
pub(crate) fn environment_for_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "mac",
        "windows" => "windows",
        _ => "ubuntu",
    }
}

/// The Anthropic native computer tool descriptor (`computer_20251124`). Rides in
/// `provider_tools`; the Anthropic Messages adapter folds `provider_tools` into
/// the same `tools` array as function tools.
pub(crate) fn anthropic_computer_tool(display_width_px: u32, display_height_px: u32) -> Value {
    json!({
        "type": "computer_20251124",
        "name": COMPUTER_TOOL_NAME,
        "display_width_px": display_width_px,
        "display_height_px": display_height_px,
        "display_number": 1,
        "enable_zoom": true,
    })
}

/// The OpenAI Responses native computer tool descriptor. GPT-5.x uses the
/// `computer` type with a desktop `environment`; the display size mirrors the
/// (unscaled) capture because OpenAI wants `original`-scaled screenshots.
pub(crate) fn openai_computer_tool(
    display_width: u32,
    display_height: u32,
    environment: &str,
) -> Value {
    json!({
        "type": "computer",
        "display_width": display_width,
        "display_height": display_height,
        "environment": environment,
    })
}

/// Whether to project the neutral computer tool onto the provider's native
/// computer-use surface. Default OFF (the universal function-tool path is used);
/// opt in with `BURIN_COMPUTER_USE_NATIVE=1|on|true` once a route's native
/// action lowering is wired.
fn native_computer_projection_enabled() -> bool {
    matches!(
        crate::stdlib::process::session_env_var("BURIN_COMPUTER_USE_NATIVE")
            .ok()
            .flatten()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "on" | "true"
    )
}

/// Project the neutral `computer` function tool onto the route's native
/// computer-use surface, in place.
///
/// - `native_anthropic` / `native_openai`: remove the plain function-schema
///   `computer` copy from `native_tools` and push the provider-native tool into
///   `provider_tools`, so the model sees exactly one computer tool (the native
///   one).
/// - any other style (`function`, `grounded`, or unset): no-op — the plain
///   function-schema tool is the generic fallback the model calls directly.
///
/// NATIVE PATH IS NOT YET COORDINATE-SAFE — that is why it is default OFF and
/// behind `BURIN_COMPUTER_USE_NATIVE`. The native tool advertises a fixed
/// `DEFAULT_DISPLAY_WIDTH` x `DEFAULT_DISPLAY_HEIGHT` (Anthropic XGA) coordinate
/// space, but three seams are NOT yet wired and must be armed together as one
/// unit before this is enabled:
///
/// 1. the captured screenshot is NOT scaled to the advertised size;
/// 2. the model's returned coordinates are NOT mapped back to the real display;
/// 3. the provider's native action vocabulary is NOT lowered to the neutral
///    `ComputerAction` the harn `computer` handler executes.
///
/// Until all three land, enabling the opt-in makes clicks land in the wrong
/// place. The default universal function-tool path uses none of this and is the
/// verified surface.
pub(crate) fn project_computer_tools(
    caps: &Capabilities,
    native_tools: &mut Option<Vec<Value>>,
    provider_tools: &mut Vec<Value>,
) {
    project_computer_tools_with(
        caps,
        native_tools,
        provider_tools,
        native_computer_projection_enabled(),
    );
}

fn project_computer_tools_with(
    caps: &Capabilities,
    native_tools: &mut Option<Vec<Value>>,
    provider_tools: &mut Vec<Value>,
    enable_native: bool,
) {
    if !enable_native {
        return;
    }
    let style = match caps.computer_use_style {
        Some(
            style @ (ComputerUseStyle::NativeAnthropic
            | ComputerUseStyle::NativeAnthropicToolset
            | ComputerUseStyle::NativeOpenai),
        ) => style,
        // Function / Grounded / none: leave the function-schema tool as-is.
        _ => return,
    };
    let Some(tools) = native_tools.as_mut() else {
        return;
    };
    if !tools.iter().any(is_computer_function_tool) {
        return;
    }
    tools.retain(|tool| !is_computer_function_tool(tool));

    // Native projection advertises XGA until capture dimensions are retained.
    let (width, height) = (DEFAULT_DISPLAY_WIDTH, DEFAULT_DISPLAY_HEIGHT);
    let native = match style {
        ComputerUseStyle::NativeAnthropic => anthropic_computer_tool(width, height),
        ComputerUseStyle::NativeAnthropicToolset => json!({"type": "computer_toolset_20260801"}),
        // NativeOpenai (the only other arm that reaches here).
        _ => openai_computer_tool(width, height, environment_for_os()),
    };
    provider_tools.push(native);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps_with_style(style: ComputerUseStyle) -> Capabilities {
        let mut caps = Capabilities::default();
        caps.computer_use_style = Some(style);
        caps
    }

    fn function_tool(name: &str) -> Value {
        json!({"type": "function", "function": {"name": name}})
    }

    #[test]
    fn anthropic_native_tool_golden_shape() {
        assert_eq!(
            anthropic_computer_tool(1024, 768),
            json!({
                "type": "computer_20251124",
                "name": "computer",
                "display_width_px": 1024,
                "display_height_px": 768,
                "display_number": 1,
                "enable_zoom": true,
            })
        );
    }

    #[test]
    fn openai_native_tool_golden_shape() {
        assert_eq!(
            openai_computer_tool(1440, 900, "mac"),
            json!({
                "type": "computer",
                "display_width": 1440,
                "display_height": 900,
                "environment": "mac",
            })
        );
    }

    #[test]
    fn projects_native_anthropic_and_suppresses_function_copy() {
        let caps = caps_with_style(ComputerUseStyle::NativeAnthropic);
        let mut native = Some(vec![function_tool("read_file"), function_tool("computer")]);
        let mut provider = Vec::new();
        project_computer_tools_with(&caps, &mut native, &mut provider, true);

        // The plain `computer` function copy is gone; other tools remain.
        let remaining = native.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(tool_name(&remaining[0]), Some("read_file"));
        // The native tool is injected into provider_tools.
        assert_eq!(provider.len(), 1);
        assert_eq!(provider[0]["type"], "computer_20251124");
        assert_eq!(provider[0]["display_width_px"], 1024);
    }

    #[test]
    fn projects_anthropic_toolset_only_when_native_projection_is_enabled() {
        let caps = caps_with_style(ComputerUseStyle::NativeAnthropicToolset);
        let mut native = Some(vec![function_tool("computer")]);
        let mut provider = Vec::new();
        project_computer_tools_with(&caps, &mut native, &mut provider, false);
        assert_eq!(native.as_ref().unwrap().len(), 1);
        assert!(provider.is_empty());
        project_computer_tools_with(&caps, &mut native, &mut provider, true);
        project_computer_tools_with(&caps, &mut native, &mut provider, true);
        assert!(native.unwrap().is_empty());
        assert_eq!(provider, vec![json!({"type": "computer_toolset_20260801"})]);
    }

    #[test]
    fn projects_native_openai_and_suppresses_function_copy() {
        let caps = caps_with_style(ComputerUseStyle::NativeOpenai);
        let mut native = Some(vec![function_tool("computer")]);
        let mut provider = Vec::new();
        project_computer_tools_with(&caps, &mut native, &mut provider, true);

        assert!(native.unwrap().is_empty());
        assert_eq!(provider.len(), 1);
        assert_eq!(provider[0]["type"], "computer");
        assert!(provider[0].get("environment").is_some());
    }

    #[test]
    fn function_style_leaves_tool_untouched() {
        for style in [ComputerUseStyle::Function, ComputerUseStyle::Grounded] {
            let caps = caps_with_style(style);
            let mut native = Some(vec![function_tool("computer")]);
            let mut provider = Vec::new();
            project_computer_tools_with(&caps, &mut native, &mut provider, true);
            assert_eq!(native.as_ref().unwrap().len(), 1, "{style:?}");
            assert!(provider.is_empty(), "{style:?}");
        }
    }

    #[test]
    fn projection_is_idempotent() {
        let caps = caps_with_style(ComputerUseStyle::NativeAnthropic);
        let mut native = Some(vec![function_tool("computer")]);
        let mut provider = Vec::new();
        project_computer_tools_with(&caps, &mut native, &mut provider, true);
        // Second pass: the native tool already lives in provider_tools and the
        // function copy is gone, so nothing changes.
        project_computer_tools_with(&caps, &mut native, &mut provider, true);
        assert!(native.unwrap().is_empty());
        assert_eq!(provider.len(), 1);
    }
}
