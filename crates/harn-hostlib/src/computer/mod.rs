//! Computer-use host capability: screenshots + mouse/keyboard control.
//!
//! This module is the single, cross-platform *execution* nucleus for
//! computer use. It is deliberately **dumb and coordinate-native**: it
//! captures the screen, executes point-based pointer/keyboard actions, reads
//! the accessibility tree, and reports OS permission status. Everything
//! *semantic* — per-provider screenshot scaling, element/mark/description
//! grounding, transcript/image assembly, approval gating, and audit — lives a
//! layer up in `harn-vm` and in the Burin host product. Keeping this split
//! means there is exactly one place that touches the OS, reused by all three
//! transports (in-process local, the macOS helper, and the cloud desktop
//! sandbox) and all three operating systems.
//!
//! ## Layering
//!
//! ```text
//!   harn-vm computer tool  ──(resolve element/mark → point, scale image)──▶
//!   hostlib_computer_* builtins ──▶ ComputerBackend ──▶ OS (xcap / enigo / AX)
//! ```
//!
//! The Harn-visible builtins are:
//! - `hostlib_computer_screenshot` → a native-resolution PNG + geometry.
//! - `hostlib_computer_execute` → run one or many coordinate-native actions.
//! - `hostlib_computer_ui_tree` → the accessibility element table (grounding).
//! - `hostlib_computer_permissions` → screen/input/accessibility grant status.
//!
//! ## Backends & features
//!
//! The trait has three transports selected at runtime by
//! `BURIN_COMPUTER_USE_TRANSPORT`:
//! - `local` — [`local::LocalBackend`] (real capture/input), gated behind the
//!   `computer-local` Cargo feature so headless clients never pull `xcap` /
//!   `enigo` and their OS toolchains.
//! - `helper` / `remote` — [`SocketBackend`], a line-delimited JSON-RPC client
//!   over a Unix socket (the macOS non-sandboxed helper) or, later, a cloud
//!   desktop sandbox. Light (serde only), always compiled with the `computer`
//!   feature.
//! - unset/unavailable → [`NullBackend`], which fails every call with a clear
//!   message so an un-provisioned environment degrades gracefully.

#[cfg(feature = "computer-local")]
pub mod local;

mod transport;

use std::sync::Arc;

use harn_vm::value::VmDictExt;
use harn_vm::VmValue;
use serde::{Deserialize, Serialize};

use crate::error::HostlibError;
use crate::registry::{BuiltinRegistry, HostlibCapability, RegisteredBuiltin, SyncHandler};
use crate::tools::args::{build_dict, dict_arg};

pub use transport::{handle_request_line, NullBackend, SocketBackend};

const MODULE: &str = "computer";
const SCREENSHOT_BUILTIN: &str = "hostlib_computer_screenshot";
const EXECUTE_BUILTIN: &str = "hostlib_computer_execute";
const UI_TREE_BUILTIN: &str = "hostlib_computer_ui_tree";
const PERMISSIONS_BUILTIN: &str = "hostlib_computer_permissions";

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    /// Primary (left) button.
    Left,
    /// Secondary (right) button.
    Right,
    /// Tertiary (middle) button.
    Middle,
}

/// Direction of a scroll action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrollDirection {
    /// Scroll up (content moves down).
    Up,
    /// Scroll down (content moves up).
    Down,
    /// Scroll left.
    Left,
    /// Scroll right.
    Right,
}

/// A single coordinate-native computer-use action.
///
/// This is the *execution* vocabulary — every target is already resolved to
/// absolute native pixels. The richer element/mark/description addressing and
/// the per-provider action projection live in `harn-vm`, which lowers to these
/// before calling the builtin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ComputerAction {
    /// Move the cursor to `(x, y)`.
    MouseMove {
        /// Absolute x in native pixels.
        x: i32,
        /// Absolute y in native pixels.
        y: i32,
    },
    /// Click a button at `(x, y)`, optionally multiple times, with modifiers.
    Click {
        /// Which button to click.
        #[serde(default = "default_left_button")]
        button: MouseButton,
        /// Absolute x in native pixels.
        x: i32,
        /// Absolute y in native pixels.
        y: i32,
        /// Number of clicks (1 = single, 2 = double, 3 = triple).
        #[serde(default = "default_click_count")]
        count: u32,
        /// Held modifier keys (e.g. `["shift"]`, `["ctrl"]`, `["super"]`).
        #[serde(default)]
        modifiers: Vec<String>,
    },
    /// Press a button down (without releasing) at `(x, y)`.
    MouseDown {
        /// Which button.
        #[serde(default = "default_left_button")]
        button: MouseButton,
        /// Absolute x in native pixels.
        x: i32,
        /// Absolute y in native pixels.
        y: i32,
    },
    /// Release a previously pressed button at `(x, y)`.
    MouseUp {
        /// Which button.
        #[serde(default = "default_left_button")]
        button: MouseButton,
        /// Absolute x in native pixels.
        x: i32,
        /// Absolute y in native pixels.
        y: i32,
    },
    /// Press at `from`, drag to `to`, release.
    Drag {
        /// Which button to hold during the drag.
        #[serde(default = "default_left_button")]
        button: MouseButton,
        /// Start x in native pixels.
        from_x: i32,
        /// Start y in native pixels.
        from_y: i32,
        /// End x in native pixels.
        to_x: i32,
        /// End y in native pixels.
        to_y: i32,
        /// Held modifier keys.
        #[serde(default)]
        modifiers: Vec<String>,
    },
    /// Scroll at `(x, y)` in `direction` by `amount` wheel steps.
    Scroll {
        /// Absolute x in native pixels.
        x: i32,
        /// Absolute y in native pixels.
        y: i32,
        /// Scroll direction.
        direction: ScrollDirection,
        /// Number of wheel steps.
        #[serde(default = "default_scroll_amount")]
        amount: i32,
        /// Held modifier keys.
        #[serde(default)]
        modifiers: Vec<String>,
    },
    /// Type a literal string.
    Type {
        /// The text to type.
        text: String,
    },
    /// Press a key or key combination, e.g. `"ctrl+s"` or `"Return"`.
    Key {
        /// The key chord (`+`-separated).
        keys: String,
    },
    /// Hold a key chord down for `duration_ms` milliseconds.
    HoldKey {
        /// The key chord.
        keys: String,
        /// How long to hold, in milliseconds.
        duration_ms: u64,
    },
    /// Pause for `duration_ms` milliseconds.
    Wait {
        /// How long to wait, in milliseconds.
        #[serde(default = "default_wait_ms")]
        duration_ms: u64,
    },
}

fn default_left_button() -> MouseButton {
    MouseButton::Left
}
fn default_click_count() -> u32 {
    1
}
fn default_scroll_amount() -> i32 {
    3
}
fn default_wait_ms() -> u64 {
    500
}

/// A captured screenshot, already resized to the advertised target size.
///
/// The `LocalBackend` resizes captures to a fixed target (default XGA
/// 1024x768) so the screenshot space, the coordinate space the model returns,
/// and the size advertised in the provider tool spec are identical — the
/// backend maps returned coordinates back to native input space internally.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScreenImage {
    /// Base64-encoded PNG bytes.
    pub base64: String,
    /// MIME type (always `image/png`).
    pub media_type: String,
    /// Image width in pixels (the advertised target width).
    pub width: u32,
    /// Image height in pixels (the advertised target height).
    pub height: u32,
    /// Backing-scale factor of the captured display (e.g. 2.0 on Retina),
    /// informational only.
    pub scale_factor: f64,
}

impl ScreenImage {
    fn into_vm(self) -> VmValue {
        let mut dict = harn_vm::value::DictMap::new();
        dict.put_str("base64", &self.base64);
        dict.put_str("media_type", &self.media_type);
        dict.put_int("width", i64::from(self.width));
        dict.put_int("height", i64::from(self.height));
        dict.put("scale_factor", VmValue::Float(self.scale_factor));
        VmValue::dict(dict)
    }
}

/// One node in the accessibility element table used for grounding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiElement {
    /// Stable reference the model addresses (opaque to the model).
    pub reference: String,
    /// Accessibility role (e.g. `AXButton`, `button`).
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

impl UiElement {
    fn into_vm(self) -> VmValue {
        let mut dict = harn_vm::value::DictMap::new();
        dict.put_str("reference", &self.reference);
        dict.put_str("role", &self.role);
        dict.put_str("name", &self.name);
        dict.put_int("x", i64::from(self.x));
        dict.put_int("y", i64::from(self.y));
        dict.put_int("width", i64::from(self.width));
        dict.put_int("height", i64::from(self.height));
        VmValue::dict(dict)
    }
}

/// The accessibility element table for the current screen.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiTree {
    /// Whether the backend produced a real tree (`false` = unsupported here).
    pub supported: bool,
    /// The flattened, interactable element table.
    pub elements: Vec<UiElement>,
}

impl UiTree {
    fn into_vm(self) -> VmValue {
        let elements: Vec<VmValue> = self.elements.into_iter().map(UiElement::into_vm).collect();
        let mut dict = harn_vm::value::DictMap::new();
        dict.put_bool("supported", self.supported);
        dict.put("elements", VmValue::List(Arc::new(elements)));
        VmValue::dict(dict)
    }
}

/// Grant state of one OS permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionState {
    /// The permission is granted.
    Granted,
    /// The permission was explicitly denied.
    Denied,
    /// The user has not yet been asked.
    Undetermined,
    /// This platform does not gate the capability (no permission needed).
    NotRequired,
    /// The backend cannot determine the state.
    Unknown,
}

impl PermissionState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Denied => "denied",
            Self::Undetermined => "undetermined",
            Self::NotRequired => "not_required",
            Self::Unknown => "unknown",
        }
    }
}

/// Aggregate permission status for computer use on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionStatus {
    /// Screen-recording / capture permission.
    pub screen: PermissionState,
    /// Synthetic-input (mouse/keyboard) permission.
    pub input: PermissionState,
    /// Accessibility-tree read permission.
    pub accessibility: PermissionState,
    /// OS identifier (`macos` / `linux` / `windows`).
    pub os: String,
    /// A human-facing hint for how to grant access on this OS.
    pub guidance: String,
}

impl PermissionStatus {
    fn into_vm(self) -> VmValue {
        let mut dict = harn_vm::value::DictMap::new();
        dict.put_str("screen", self.screen.as_str());
        dict.put_str("input", self.input.as_str());
        dict.put_str("accessibility", self.accessibility.as_str());
        dict.put_str("os", &self.os);
        dict.put_str("guidance", &self.guidance);
        dict.put_bool(
            "ready",
            matches!(
                self.screen,
                PermissionState::Granted | PermissionState::NotRequired
            ) && matches!(
                self.input,
                PermissionState::Granted | PermissionState::NotRequired
            ),
        );
        VmValue::dict(dict)
    }
}

/// What a backend can do. Advertised so `harn-vm` can gate the tool surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendCapabilities {
    /// Backend identifier (`local` / `helper` / `remote` / `null`).
    pub name: String,
    /// Whether screenshots are supported.
    pub screenshot: bool,
    /// Whether synthetic input is supported.
    pub input: bool,
    /// Whether an accessibility tree is available.
    pub ui_tree: bool,
}

/// The cross-platform, cross-transport execution contract.
///
/// Implementations must be `Send + Sync` — the capability shares one behind an
/// `Arc` across the VM's builtin closures. All methods are synchronous: capture
/// and input are blocking OS calls, and the hostlib registry only wires sync
/// handlers.
pub trait ComputerBackend: Send + Sync {
    /// Advertise what this backend supports.
    fn capabilities(&self) -> BackendCapabilities;
    /// Capture the primary display at native resolution.
    fn screenshot(&self) -> Result<ScreenImage, String>;
    /// Execute actions in order. A failure aborts the remaining actions.
    fn execute(&self, actions: &[ComputerAction]) -> Result<(), String>;
    /// Read the accessibility element table for grounding.
    fn ui_tree(&self) -> Result<UiTree, String>;
    /// Report OS permission status.
    fn permissions(&self) -> Result<PermissionStatus, String>;
}

/// The computer-use hostlib capability. Owns one shared backend.
pub struct ComputerUseCapability {
    backend: Arc<dyn ComputerBackend>,
}

impl ComputerUseCapability {
    /// Construct with the backend selected by `BURIN_COMPUTER_USE_TRANSPORT`
    /// (`local` default when compiled; `helper`/`remote` for the socket
    /// transport; `none`/unavailable → a graceful [`NullBackend`]).
    pub fn new() -> Self {
        Self {
            backend: select_backend(),
        }
    }

    /// Construct with an explicit backend (used by embedders and tests).
    pub fn with_backend(backend: Arc<dyn ComputerBackend>) -> Self {
        Self { backend }
    }
}

impl Default for ComputerUseCapability {
    fn default() -> Self {
        Self::new()
    }
}

fn select_backend() -> Arc<dyn ComputerBackend> {
    let transport = std::env::var("BURIN_COMPUTER_USE_TRANSPORT").unwrap_or_default();
    match transport.as_str() {
        "helper" | "remote" => match SocketBackend::from_env(&transport) {
            Ok(backend) => Arc::new(backend),
            Err(message) => Arc::new(NullBackend::new(message)),
        },
        "none" => Arc::new(NullBackend::new(
            "computer use is disabled (BURIN_COMPUTER_USE_TRANSPORT=none)".to_string(),
        )),
        "" | "local" => default_local_backend(),
        other => Arc::new(NullBackend::new(format!(
            "unknown BURIN_COMPUTER_USE_TRANSPORT '{other}' (expected local|helper|remote|none)"
        ))),
    }
}

#[cfg(feature = "computer-local")]
fn default_local_backend() -> Arc<dyn ComputerBackend> {
    Arc::new(local::LocalBackend::new())
}

#[cfg(not(feature = "computer-local"))]
fn default_local_backend() -> Arc<dyn ComputerBackend> {
    Arc::new(NullBackend::new(
        "local computer-use backend is not compiled in (enable the `computer-local` feature)"
            .to_string(),
    ))
}

impl HostlibCapability for ComputerUseCapability {
    fn module_name(&self) -> &'static str {
        MODULE
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        let backend = self.backend.clone();
        let handler: SyncHandler = {
            let backend = backend.clone();
            Arc::new(move |_args: &[VmValue]| screenshot_builtin(backend.as_ref()))
        };
        registry.register(RegisteredBuiltin {
            name: SCREENSHOT_BUILTIN,
            module: MODULE,
            method: "screenshot",
            handler,
        });

        let handler: SyncHandler = {
            let backend = backend.clone();
            Arc::new(move |args: &[VmValue]| execute_builtin(backend.as_ref(), args))
        };
        registry.register(RegisteredBuiltin {
            name: EXECUTE_BUILTIN,
            module: MODULE,
            method: "execute",
            handler,
        });

        let handler: SyncHandler = {
            let backend = backend.clone();
            Arc::new(move |_args: &[VmValue]| ui_tree_builtin(backend.as_ref()))
        };
        registry.register(RegisteredBuiltin {
            name: UI_TREE_BUILTIN,
            module: MODULE,
            method: "ui_tree",
            handler,
        });

        let handler: SyncHandler =
            Arc::new(move |_args: &[VmValue]| permissions_builtin(backend.as_ref()));
        registry.register(RegisteredBuiltin {
            name: PERMISSIONS_BUILTIN,
            module: MODULE,
            method: "permissions",
            handler,
        });
    }
}

fn backend_error(builtin: &'static str, message: String) -> HostlibError {
    HostlibError::Backend { builtin, message }
}

fn screenshot_builtin(backend: &dyn ComputerBackend) -> Result<VmValue, HostlibError> {
    let image = backend
        .screenshot()
        .map_err(|message| backend_error(SCREENSHOT_BUILTIN, message))?;
    Ok(image.into_vm())
}

fn execute_builtin(
    backend: &dyn ComputerBackend,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let dict = dict_arg(EXECUTE_BUILTIN, args)?;
    let actions = parse_actions(&dict)?;
    let count = actions.len();
    backend
        .execute(&actions)
        .map_err(|message| backend_error(EXECUTE_BUILTIN, message))?;
    Ok(build_dict([("executed", VmValue::Int(count as i64))]))
}

fn ui_tree_builtin(backend: &dyn ComputerBackend) -> Result<VmValue, HostlibError> {
    let tree = backend
        .ui_tree()
        .map_err(|message| backend_error(UI_TREE_BUILTIN, message))?;
    Ok(tree.into_vm())
}

fn permissions_builtin(backend: &dyn ComputerBackend) -> Result<VmValue, HostlibError> {
    let status = backend
        .permissions()
        .map_err(|message| backend_error(PERMISSIONS_BUILTIN, message))?;
    Ok(status.into_vm())
}

/// Parse the `{actions: [...]}` (or a single `{action: ...}`) payload into the
/// neutral action list. Actions are decoded through `serde_json` so the union
/// stays defined in exactly one place (the `#[serde(tag = "action")]` enum).
fn parse_actions(dict: &harn_vm::value::DictMap) -> Result<Vec<ComputerAction>, HostlibError> {
    let json = vm_dict_to_json(dict);
    let obj = json
        .as_object()
        .ok_or_else(|| HostlibError::InvalidParameter {
            builtin: EXECUTE_BUILTIN,
            param: "params",
            message: "expected a dict payload".to_string(),
        })?;

    let raw_actions: Vec<serde_json::Value> = if let Some(list) = obj.get("actions") {
        list.as_array()
            .ok_or_else(|| HostlibError::InvalidParameter {
                builtin: EXECUTE_BUILTIN,
                param: "actions",
                message: "expected a list of actions".to_string(),
            })?
            .clone()
    } else if obj.contains_key("action") {
        vec![json.clone()]
    } else {
        return Err(HostlibError::MissingParameter {
            builtin: EXECUTE_BUILTIN,
            param: "actions",
        });
    };

    raw_actions
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            serde_json::from_value::<ComputerAction>(value).map_err(|err| {
                HostlibError::InvalidParameter {
                    builtin: EXECUTE_BUILTIN,
                    param: "actions",
                    message: format!("action at index {index} is invalid: {err}"),
                }
            })
        })
        .collect()
}

/// Convert the subset of `VmValue` used by action payloads into
/// `serde_json::Value`. Dependency-free (does not rely on the feature-gated
/// `llm` bridge) so the neutral action union stays defined in one place — the
/// `#[serde(tag = "action")]` enum — while lean hostlib builds still compile.
fn vm_value_to_json(value: &VmValue) -> serde_json::Value {
    match value {
        VmValue::Nil => serde_json::Value::Null,
        VmValue::Bool(b) => serde_json::Value::Bool(*b),
        VmValue::Int(n) => serde_json::Value::from(*n),
        VmValue::Float(n) => serde_json::Number::from_f64(*n)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        VmValue::String(s) => serde_json::Value::String(s.to_string()),
        VmValue::List(items) => {
            serde_json::Value::Array(items.iter().map(vm_value_to_json).collect())
        }
        VmValue::Dict(dict) => vm_dict_to_json(dict),
        _ => serde_json::Value::Null,
    }
}

fn vm_dict_to_json(dict: &harn_vm::value::DictMap) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (key, value) in dict.iter() {
        map.insert(key.as_str().to_string(), vm_value_to_json(value));
    }
    serde_json::Value::Object(map)
}

/// Split a `+`-separated key chord (e.g. `"ctrl+shift+s"`) into its parts,
/// lower-cased and trimmed. Shared by every backend that maps chords to OS key
/// events, so the chord grammar lives in exactly one place.
pub fn split_chord(chord: &str) -> Vec<String> {
    chord
        .split('+')
        .map(|part| part.trim().to_ascii_lowercase())
        .filter(|part| !part.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_deserializes_with_defaults() {
        let value = serde_json::json!({"action": "click", "x": 10, "y": 20});
        let action: ComputerAction = serde_json::from_value(value).expect("parse");
        assert_eq!(
            action,
            ComputerAction::Click {
                button: MouseButton::Left,
                x: 10,
                y: 20,
                count: 1,
                modifiers: vec![],
            }
        );
    }

    #[test]
    fn scroll_and_type_roundtrip() {
        let value = serde_json::json!({
            "action": "scroll", "x": 5, "y": 6,
            "direction": "down", "amount": 4, "modifiers": ["shift"]
        });
        let action: ComputerAction = serde_json::from_value(value).expect("parse");
        assert_eq!(
            action,
            ComputerAction::Scroll {
                x: 5,
                y: 6,
                direction: ScrollDirection::Down,
                amount: 4,
                modifiers: vec!["shift".to_string()],
            }
        );

        let typed: ComputerAction =
            serde_json::from_value(serde_json::json!({"action": "type", "text": "hi"}))
                .expect("parse");
        assert_eq!(typed, ComputerAction::Type { text: "hi".into() });
    }

    #[test]
    fn split_chord_normalizes() {
        assert_eq!(split_chord("Ctrl+Shift+S"), vec!["ctrl", "shift", "s"]);
        assert_eq!(split_chord(" cmd + a "), vec!["cmd", "a"]);
        assert!(split_chord("").is_empty());
    }

    #[test]
    fn null_backend_fails_cleanly() {
        let backend = NullBackend::new("no backend".to_string());
        assert!(backend.screenshot().is_err());
        assert!(!backend.capabilities().screenshot);
    }
}
