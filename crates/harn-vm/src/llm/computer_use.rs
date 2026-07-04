//! Neutral computer-use tool projection and geometry helpers.
//!
//! harn-vm owns the *semantic* layer of computer use. It projects the single
//! neutral `computer` function tool onto each provider's native computer-use
//! surface, scales screenshots per provider, maps model-space coordinates back
//! to native pixels, and resolves element/mark grounding targets to points.
//!
//! The coordinate-native *execution* nucleus (screenshot capture, pointer /
//! keyboard input, accessibility tree) lives in `harn-hostlib`'s `computer`
//! module and is reached through the `hostlib_computer_*` builtins. Nothing in
//! this module touches the OS — it is pure projection and geometry so it stays
//! deterministic and unit-testable.
//!
//! ## De-overfitting
//!
//! There is exactly one host-facing `computer` tool. Per provider, this module
//! lowers it to:
//! - `native_anthropic` → the Anthropic `computer_20251124` tool,
//! - `native_openai` → the OpenAI Responses `computer` tool,
//! - `function` / `grounded` / unset → left as the plain function-schema tool
//!   (the generic fallback), untouched.

use serde_json::{json, Value};

use crate::llm::capabilities::Capabilities;

/// Audit topic under which computer-use actions are recorded. Mirrors the
/// vision OCR audit topic pattern (`crate::stdlib::vision::VISION_OCR_AUDIT_TOPIC`).
///
/// Actual audit emission (one record per executed action, with the resolved
/// native coordinates and the grounding target that produced them) is wired by
/// the agent loop / host executor; this constant is the single canonical topic
/// string those emitters key on.
// Tested pure API awaiting live wiring (audit emission is a follow-up seam).
#[allow(dead_code)]
pub(crate) const COMPUTER_USE_AUDIT_TOPIC: &str = "audit.computer_use";

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
/// INTEGRATION SEAM — display size: the native tool advertises, and the
/// screenshot is scaled to, `DEFAULT_DISPLAY_WIDTH` x `DEFAULT_DISPLAY_HEIGHT`
/// (Anthropic XGA) until the real captured display size is threaded here. The
/// orchestrator should pass the captured `ScreenImage { width, height }`
/// (scaled per [`scale_screenshot`]) so the advertised size matches the image
/// the model actually receives.
/// Whether to project the neutral computer tool onto the provider's native
/// computer-use surface. Default OFF (the universal function-tool path is used);
/// opt in with `BURIN_COMPUTER_USE_NATIVE=1|on|true` once a route's native
/// action lowering is wired.
fn native_computer_projection_enabled() -> bool {
    matches!(
        std::env::var("BURIN_COMPUTER_USE_NATIVE")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "on" | "true"
    )
}

pub(crate) fn project_computer_tools(
    caps: &Capabilities,
    native_tools: &mut Option<Vec<Value>>,
    provider_tools: &mut Vec<Value>,
) {
    // Native provider computer tools (Anthropic `computer_20251124`, OpenAI
    // Responses `computer`) are an OPT-IN optimization, not the default. The
    // universal path is the plain function-schema `computer` tool + the neutral
    // screenshot round-trip: it uses ONE action schema (x/y, not the provider's
    // `coordinate[]`), works on every vision model regardless of whether the
    // provider supports native computer use (so a cheap model never 400s on an
    // unsupported `computer_20251124`), and carries screenshots back through the
    // verified image-block path. Native projection also swaps in the provider's
    // own action vocabulary, which the harn `computer` handler does not parse —
    // so keep it behind an explicit opt-in until that lowering is wired.
    project_computer_tools_with(
        caps,
        native_tools,
        provider_tools,
        native_computer_projection_enabled(),
    )
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
    let style = match caps.computer_use_style.as_deref() {
        Some(style @ ("native_anthropic" | "native_openai")) => style,
        // `function` / `grounded` / none: leave the function-schema tool as-is.
        _ => return,
    };
    let Some(tools) = native_tools.as_mut() else {
        return;
    };
    if !tools.iter().any(is_computer_function_tool) {
        return;
    }
    tools.retain(|tool| !is_computer_function_tool(tool));

    // See the INTEGRATION SEAM note above: default to XGA until real dims flow.
    let (width, height) = (DEFAULT_DISPLAY_WIDTH, DEFAULT_DISPLAY_HEIGHT);
    let native = match style {
        "native_anthropic" => anthropic_computer_tool(width, height),
        // native_openai
        _ => openai_computer_tool(width, height, environment_for_os()),
    };
    provider_tools.push(native);
}

/// Fit `(width, height)` within `(max_w, max_h)` preserving aspect ratio,
/// never upscaling. Zero-sized inputs pass through unchanged.
#[allow(dead_code)] // used by scale_screenshot (a follow-up live-wiring seam).
fn fit_within(width: u32, height: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (width, height);
    }
    if width <= max_w && height <= max_h {
        return (width, height);
    }
    let scale = (f64::from(max_w) / f64::from(width)).min(f64::from(max_h) / f64::from(height));
    let scaled_w = ((f64::from(width) * scale).round() as u32).max(1);
    let scaled_h = ((f64::from(height) * scale).round() as u32).max(1);
    (scaled_w, scaled_h)
}

/// Scale a native screenshot to the model-facing target size for `style`.
///
/// - `xga` (Anthropic): fit within 1024x768 preserving aspect ratio, never
///   upscaling.
/// - `original` / `none` / unknown / unset (OpenAI et al.): identity.
//
// Items 5/6: these geometry + grounding helpers are the tested pure API that
// the orchestrator wires into the live screenshot/coordinate flow as a
// follow-up (see the INTEGRATION SEAM notes). `#[allow(dead_code)]` keeps the
// crate warning-clean until that wiring lands.
#[allow(dead_code)]
pub(crate) fn scale_screenshot(width: u32, height: u32, style: Option<&str>) -> (u32, u32) {
    match style {
        Some("xga") => fit_within(width, height, DEFAULT_DISPLAY_WIDTH, DEFAULT_DISPLAY_HEIGHT),
        _ => (width, height),
    }
}

/// Map a native coordinate into the model-facing target (scaled) space. Inverse
/// of [`map_coord_back`]. A zero native dimension passes the axis through.
#[allow(dead_code)]
pub(crate) fn map_coord_to_target(
    native_xy: (i32, i32),
    native_dims: (u32, u32),
    target_dims: (u32, u32),
) -> (i32, i32) {
    let (nx, ny) = native_xy;
    let (nw, nh) = native_dims;
    let (tw, th) = target_dims;
    let mx = if nw == 0 {
        nx
    } else {
        (f64::from(nx) * f64::from(tw) / f64::from(nw)).round() as i32
    };
    let my = if nh == 0 {
        ny
    } else {
        (f64::from(ny) * f64::from(th) / f64::from(nh)).round() as i32
    };
    (mx, my)
}

/// Map a model-space coordinate (expressed in the target/scaled dims the model
/// saw) back to absolute native pixels for the execution nucleus. A zero target
/// dimension passes the axis through unchanged.
///
/// INTEGRATION SEAM — live flow: the orchestrator should [`scale_screenshot`]
/// the captured image before sending it, remember the `(target_dims,
/// native_dims)` pair, and run every model-returned click/point through this
/// function before lowering to the coordinate-native `hostlib_computer_execute`
/// action list.
#[allow(dead_code)]
pub(crate) fn map_coord_back(
    model_xy: (i32, i32),
    target_dims: (u32, u32),
    native_dims: (u32, u32),
) -> (i32, i32) {
    let (mx, my) = model_xy;
    let (tw, th) = target_dims;
    let (nw, nh) = native_dims;
    let nx = if tw == 0 {
        mx
    } else {
        (f64::from(mx) * f64::from(nw) / f64::from(tw)).round() as i32
    };
    let ny = if th == 0 {
        my
    } else {
        (f64::from(my) * f64::from(nh) / f64::from(th)).round() as i32
    };
    (nx, ny)
}

/// One row of the accessibility element table used for grounding. Mirrors the
/// hostlib `UiElement` shape (`reference`, `role`, `name`, bbox) so callers can
/// build these directly from a `hostlib_computer_ui_tree` result.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct GroundingElement {
    /// Stable reference the model addresses.
    pub reference: String,
    /// Accessibility role (e.g. `AXButton`).
    pub role: String,
    /// Accessible name / label.
    pub name: String,
    /// Bounding-box x in native pixels.
    pub x: i32,
    /// Bounding-box y in native pixels.
    pub y: i32,
    /// Bounding-box width in native pixels.
    pub width: i32,
    /// Bounding-box height in native pixels.
    pub height: i32,
}

/// A grounding target a model may address instead of raw coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum GroundingTarget {
    /// Address an element by its stable `reference`.
    Element {
        /// The element's `reference`.
        reference: String,
    },
    /// Address a set-of-marks id (matched against `reference`, or a 1-based
    /// index into the element table when the id is numeric).
    Mark {
        /// The mark id.
        id: String,
    },
    /// A raw native point (pass-through).
    Point {
        /// Absolute x in native pixels.
        x: i32,
        /// Absolute y in native pixels.
        y: i32,
    },
}

/// The native center point of an element's bounding box.
#[allow(dead_code)]
fn bbox_center(element: &GroundingElement) -> (i32, i32) {
    (
        element.x + element.width / 2,
        element.y + element.height / 2,
    )
}

/// Resolve a grounding target to a native `(x, y)` point.
///
/// - `Point` returns its coordinates unchanged.
/// - `Element` returns the bbox center of the element whose `reference`
///   matches.
/// - `Mark` returns the bbox center of the element whose `reference` matches
///   the id, or (when the id is a positive integer) the 1-based index into the
///   element table.
///
/// Returns `None` when an element/mark target does not resolve.
#[allow(dead_code)]
pub(crate) fn resolve_grounding(
    elements: &[GroundingElement],
    target: &GroundingTarget,
) -> Option<(i32, i32)> {
    match target {
        GroundingTarget::Point { x, y } => Some((*x, *y)),
        GroundingTarget::Element { reference } => elements
            .iter()
            .find(|element| &element.reference == reference)
            .map(bbox_center),
        GroundingTarget::Mark { id } => elements
            .iter()
            .find(|element| &element.reference == id)
            .or_else(|| {
                id.parse::<usize>()
                    .ok()
                    .filter(|index| *index >= 1)
                    .and_then(|index| elements.get(index - 1))
            })
            .map(bbox_center),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps_with_style(style: &str) -> Capabilities {
        Capabilities {
            computer_use_style: Some(style.to_string()),
            ..Capabilities::default()
        }
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
        let caps = caps_with_style("native_anthropic");
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
    fn projects_native_openai_and_suppresses_function_copy() {
        let caps = caps_with_style("native_openai");
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
        for style in ["function", "grounded"] {
            let caps = caps_with_style(style);
            let mut native = Some(vec![function_tool("computer")]);
            let mut provider = Vec::new();
            project_computer_tools_with(&caps, &mut native, &mut provider, true);
            assert_eq!(native.as_ref().unwrap().len(), 1, "{style}");
            assert!(provider.is_empty(), "{style}");
        }
    }

    #[test]
    fn projection_is_idempotent() {
        let caps = caps_with_style("native_anthropic");
        let mut native = Some(vec![function_tool("computer")]);
        let mut provider = Vec::new();
        project_computer_tools_with(&caps, &mut native, &mut provider, true);
        // Second pass: the native tool already lives in provider_tools and the
        // function copy is gone, so nothing changes.
        project_computer_tools_with(&caps, &mut native, &mut provider, true);
        assert!(native.unwrap().is_empty());
        assert_eq!(provider.len(), 1);
    }

    #[test]
    fn xga_scaling_fits_and_original_is_identity() {
        // 1920x1080 fits within 1024x768 → 1024x576 (letterboxed by width).
        assert_eq!(scale_screenshot(1920, 1080, Some("xga")), (1024, 576));
        // Already small: no upscaling.
        assert_eq!(scale_screenshot(800, 600, Some("xga")), (800, 600));
        // original / none / unset: identity.
        assert_eq!(scale_screenshot(1920, 1080, Some("original")), (1920, 1080));
        assert_eq!(scale_screenshot(1920, 1080, None), (1920, 1080));
    }

    #[test]
    fn coordinate_roundtrip_within_one_pixel() {
        let native_dims = (1920, 1080);
        let target_dims = scale_screenshot(native_dims.0, native_dims.1, Some("xga"));
        for native in [(0, 0), (960, 540), (1919, 1079), (100, 999)] {
            let model = map_coord_to_target(native, native_dims, target_dims);
            let back = map_coord_back(model, target_dims, native_dims);
            assert!(
                (back.0 - native.0).abs() <= 1 && (back.1 - native.1).abs() <= 1,
                "native {native:?} -> model {model:?} -> back {back:?}"
            );
        }
    }

    #[test]
    fn original_scaling_coordinate_identity() {
        let dims = (1440, 900);
        let target = scale_screenshot(dims.0, dims.1, Some("original"));
        assert_eq!(target, dims);
        assert_eq!(map_coord_back((123, 456), target, dims), (123, 456));
    }

    #[test]
    fn grounding_resolves_element_mark_and_point() {
        let elements = vec![
            GroundingElement {
                reference: "el-a".to_string(),
                role: "AXButton".to_string(),
                name: "OK".to_string(),
                x: 100,
                y: 200,
                width: 40,
                height: 20,
            },
            GroundingElement {
                reference: "el-b".to_string(),
                role: "AXTextField".to_string(),
                name: "Search".to_string(),
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            },
        ];
        // Element by reference → bbox center.
        assert_eq!(
            resolve_grounding(
                &elements,
                &GroundingTarget::Element {
                    reference: "el-a".to_string()
                }
            ),
            Some((120, 210))
        );
        // Mark by 1-based index → element el-b center.
        assert_eq!(
            resolve_grounding(
                &elements,
                &GroundingTarget::Mark {
                    id: "2".to_string()
                }
            ),
            Some((5, 5))
        );
        // Point pass-through.
        assert_eq!(
            resolve_grounding(&elements, &GroundingTarget::Point { x: 7, y: 9 }),
            Some((7, 9))
        );
        // Unknown element → None.
        assert_eq!(
            resolve_grounding(
                &elements,
                &GroundingTarget::Element {
                    reference: "nope".to_string()
                }
            ),
            None
        );
    }

    #[test]
    fn audit_topic_is_stable() {
        assert_eq!(COMPUTER_USE_AUDIT_TOPIC, "audit.computer_use");
    }
}
