//! In-process local backend: real screen capture (`xcap`) and synthetic input
//! (`enigo`), plus per-OS permission preflight. Compiled only under the
//! `computer-local` Cargo feature so headless clients never pull the OS
//! capture/input toolchains.

use std::io::Cursor;
use std::sync::Mutex;

use base64::Engine as _;
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard as _, Mouse as _, Settings};

use super::{
    split_chord, BackendCapabilities, ComputerAction, ComputerBackend, Modifier, MouseButton,
    PermissionState, PermissionStatus, ScreenImage, ScrollDirection, UiTree,
};

/// Default screenshot BOX. The capture is fitted INSIDE this box preserving
/// aspect ratio (never upscaled), so the advertised screenshot size is the
/// fitted size, and the screenshot space == the coordinate space the model
/// returns == that fitted size — no cross-call coordinate state. The box is
/// sized to stay under Anthropic's vision limits (long edge <= 1568 px, total
/// <= ~1.15 MP) so the provider does not re-downscale and blur the frame; for a
/// 16:9 display this fits to 1400x787 (~1.1 MP). Bigger than the old 1024x768
/// XGA so dense UI text survives the downscale. Overridable via
/// `BURIN_COMPUTER_USE_WIDTH` / `BURIN_COMPUTER_USE_HEIGHT`.
const DEFAULT_TARGET_WIDTH: u32 = 1400;
const DEFAULT_TARGET_HEIGHT: u32 = 1050;

/// Upper bound on a model-requested `wait` / `hold_key` duration. The duration
/// is model-controlled, so a hallucinated or adversarial value (e.g. `u64::MAX`
/// ms) must not wedge the synchronous executor thread or pin keys down forever —
/// every sleep is clamped to this cap.
const MAX_ACTION_DURATION_MS: u64 = 60_000;

/// Resolve the target screenshot box from the environment, falling back to the
/// default. The capture is fitted INSIDE this box (aspect-preserving).
fn target_dims() -> (u32, u32) {
    let read = |key: &str, fallback: u32| {
        std::env::var(key)
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(fallback)
    };
    (
        read("BURIN_COMPUTER_USE_WIDTH", DEFAULT_TARGET_WIDTH),
        read("BURIN_COMPUTER_USE_HEIGHT", DEFAULT_TARGET_HEIGHT),
    )
}

/// Fit `(width, height)` inside `(max_w, max_h)` preserving aspect ratio and
/// never upscaling. Returns at least 1x1. This keeps the screenshot's geometry
/// faithful to the display instead of stretching a 16:9 screen into a 4:3 box.
fn fit_within(width: u32, height: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (width.max(1), height.max(1));
    }
    let scale_w = f64::from(max_w) / f64::from(width);
    let scale_h = f64::from(max_h) / f64::from(height);
    let scale = scale_w.min(scale_h).min(1.0);
    let out_w = ((f64::from(width) * scale).round() as u32).max(1);
    let out_h = ((f64::from(height) * scale).round() as u32).max(1);
    (out_w, out_h)
}

/// Local capture/input backend. Cheap to construct; captures a fresh `Enigo`
/// per action batch and enumerates monitors per screenshot.
pub struct LocalBackend {
    /// Cached transform from screenshot (target) space to the logical-point
    /// space `enigo` expects, as `(x_ratio, y_ratio)` where
    /// `logical = target * ratio`. `None` until the first screenshot establishes
    /// the display geometry: a coordinate action issued before any screenshot
    /// has no valid transform, so it errors rather than silently applying an
    /// identity map (which would mislocate every click on a Retina display).
    transform: Mutex<Option<(f64, f64)>>,
}

impl LocalBackend {
    /// Construct a local backend.
    pub fn new() -> Self {
        Self {
            transform: Mutex::new(None),
        }
    }

    /// Map a screenshot-space coordinate (target pixels, what the model sees and
    /// returns) into the logical-point space `enigo` uses for input. Errors if no
    /// screenshot has established the transform yet — the model must observe the
    /// screen before it can act on coordinates.
    fn to_input_coords(&self, x: i32, y: i32) -> Result<(i32, i32), String> {
        let (rx, ry) = self
            .transform
            .lock()
            .expect("transform mutex")
            .ok_or_else(|| {
                "no coordinate transform yet — take a screenshot before issuing a coordinate \
                 action so the display geometry is known"
                    .to_string()
            })?;
        Ok((
            (f64::from(x) * rx).round() as i32,
            (f64::from(y) * ry).round() as i32,
        ))
    }
}

impl Default for LocalBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl ComputerBackend for LocalBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            name: "local".to_string(),
            screenshot: true,
            input: true,
            // Accessibility grounding is not implemented per-OS yet; grounding
            // degrades to set-of-marks / raw coordinates upstream.
            ui_tree: false,
        }
    }

    fn screenshot(&self) -> Result<ScreenImage, String> {
        use xcap::Monitor;

        let mut monitors = Monitor::all().map_err(|err| format!("enumerate monitors: {err}"))?;
        if monitors.is_empty() {
            return Err("no monitor found".to_string());
        }
        // The primary if we can identify one, else the first — enumerated once.
        let index = monitors
            .iter()
            .position(|m| m.is_primary().unwrap_or(false))
            .unwrap_or(0);
        let monitor = monitors.swap_remove(index);

        let captured = monitor
            .capture_image()
            .map_err(|err| format!("capture screen: {err}"))?;
        let physical_width = captured.width();
        let physical_height = captured.height();

        // Fit the capture inside the target box PRESERVING ASPECT RATIO. The old
        // code resized to EXACTLY the box (e.g. a 16:9 5K display squished into
        // 4:3 1024x768), which distorted every window and, combined with a
        // bilinear filter at ~5x downscale, turned dense UI text into aliased
        // mush the model could not read. Fitting keeps geometry correct, and
        // Lanczos3 (a windowed-sinc kernel) preserves far more high-frequency
        // detail — i.e. small text — than Triangle when downscaling by a large
        // factor. The box defaults stay under Anthropic's vision limits (<=1568
        // px long edge, ~1.15 MP) so the provider does not re-downscale.
        let (box_width, box_height) = target_dims();
        let (target_width, target_height) =
            fit_within(physical_width, physical_height, box_width, box_height);
        let resized = image::imageops::resize(
            &captured,
            target_width,
            target_height,
            image::imageops::FilterType::Lanczos3,
        );

        // enigo takes input in logical points; target maps to logical by
        // logical_dim / target_dim (this folds in the Retina backing scale).
        let logical_width = monitor.width().unwrap_or(physical_width).max(1);
        let logical_height = monitor.height().unwrap_or(captured.height()).max(1);
        *self.transform.lock().expect("transform mutex") = Some((
            f64::from(logical_width) / f64::from(target_width),
            f64::from(logical_height) / f64::from(target_height),
        ));
        // Prefer xcap's per-OS backing scale (correct on Windows fractional
        // scaling, where `monitor.width()` returns physical pels so the
        // physical/logical ratio would collapse to ~1.0). Fall back to the ratio
        // if the platform can't report it.
        let scale_factor = monitor
            .scale_factor()
            .map(f64::from)
            .unwrap_or_else(|_| f64::from(physical_width) / f64::from(logical_width));

        let mut png = Vec::new();
        resized
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|err| format!("encode png: {err}"))?;
        let base64 = base64::engine::general_purpose::STANDARD.encode(&png);

        Ok(ScreenImage {
            base64,
            media_type: "image/png".to_string(),
            width: target_width,
            height: target_height,
            scale_factor,
        })
    }

    fn execute(&self, actions: &[ComputerAction]) -> Result<(), String> {
        let mut enigo =
            Enigo::new(&Settings::default()).map_err(|err| format!("init input: {err}"))?;
        for action in actions {
            self.run_action(&mut enigo, action)?;
        }
        Ok(())
    }

    fn ui_tree(&self) -> Result<UiTree, String> {
        // Per-OS accessibility trees (macOS AX / Windows UIA / Linux AT-SPI)
        // are a follow-up; report "unsupported" so grounding degrades cleanly.
        Ok(UiTree::default())
    }

    fn permissions(&self) -> Result<PermissionStatus, String> {
        Ok(platform_permissions())
    }
}

impl LocalBackend {
    fn run_action(&self, enigo: &mut Enigo, action: &ComputerAction) -> Result<(), String> {
        match action {
            ComputerAction::MouseMove { x, y } => {
                let (x, y) = self.to_input_coords(*x, *y)?;
                enigo
                    .move_mouse(x, y, Coordinate::Abs)
                    .map_err(|err| format!("move_mouse: {err}"))
            }
            ComputerAction::Click {
                button,
                x,
                y,
                count,
                modifiers,
            } => {
                let (x, y) = self.to_input_coords(*x, *y)?;
                enigo
                    .move_mouse(x, y, Coordinate::Abs)
                    .map_err(|err| format!("move_mouse: {err}"))?;
                with_modifiers(enigo, &modifier_key_names(modifiers), |enigo| {
                    for _ in 0..(*count).max(1) {
                        enigo
                            .button(to_button(*button), Direction::Click)
                            .map_err(|err| format!("button click: {err}"))?;
                    }
                    Ok(())
                })
            }
            ComputerAction::MouseDown { button, x, y } => {
                let (x, y) = self.to_input_coords(*x, *y)?;
                enigo
                    .move_mouse(x, y, Coordinate::Abs)
                    .map_err(|err| format!("move_mouse: {err}"))?;
                enigo
                    .button(to_button(*button), Direction::Press)
                    .map_err(|err| format!("button press: {err}"))
            }
            ComputerAction::MouseUp { button, x, y } => {
                let (x, y) = self.to_input_coords(*x, *y)?;
                enigo
                    .move_mouse(x, y, Coordinate::Abs)
                    .map_err(|err| format!("move_mouse: {err}"))?;
                enigo
                    .button(to_button(*button), Direction::Release)
                    .map_err(|err| format!("button release: {err}"))
            }
            ComputerAction::Drag {
                button,
                from_x,
                from_y,
                to_x,
                to_y,
                modifiers,
            } => {
                let (fx, fy) = self.to_input_coords(*from_x, *from_y)?;
                let (tx, ty) = self.to_input_coords(*to_x, *to_y)?;
                with_modifiers(enigo, &modifier_key_names(modifiers), |enigo| {
                    enigo
                        .move_mouse(fx, fy, Coordinate::Abs)
                        .map_err(|err| format!("move_mouse: {err}"))?;
                    enigo
                        .button(to_button(*button), Direction::Press)
                        .map_err(|err| format!("drag press: {err}"))?;
                    enigo
                        .move_mouse(tx, ty, Coordinate::Abs)
                        .map_err(|err| format!("drag move: {err}"))?;
                    enigo
                        .button(to_button(*button), Direction::Release)
                        .map_err(|err| format!("drag release: {err}"))
                })
            }
            ComputerAction::Scroll {
                x,
                y,
                direction,
                amount,
                modifiers,
            } => {
                let (x, y) = self.to_input_coords(*x, *y)?;
                enigo
                    .move_mouse(x, y, Coordinate::Abs)
                    .map_err(|err| format!("move_mouse: {err}"))?;
                let (axis, magnitude) = match direction {
                    ScrollDirection::Down => (Axis::Vertical, *amount),
                    ScrollDirection::Up => (Axis::Vertical, -*amount),
                    ScrollDirection::Right => (Axis::Horizontal, *amount),
                    ScrollDirection::Left => (Axis::Horizontal, -*amount),
                };
                with_modifiers(enigo, &modifier_key_names(modifiers), |enigo| {
                    enigo
                        .scroll(magnitude, axis)
                        .map_err(|err| format!("scroll: {err}"))
                })
            }
            ComputerAction::Type { text } => {
                enigo.text(text).map_err(|err| format!("type text: {err}"))
            }
            ComputerAction::Key { keys } => press_chord(enigo, keys),
            ComputerAction::HoldKey { keys, duration_ms } => {
                let parts = split_chord(keys);
                let resolved: Vec<Key> = parts
                    .iter()
                    .map(|p| parse_key(p).ok_or_else(|| format!("unknown key '{p}'")))
                    .collect::<Result<_, _>>()?;
                for key in &resolved {
                    enigo
                        .key(*key, Direction::Press)
                        .map_err(|err| format!("key press: {err}"))?;
                }
                std::thread::sleep(std::time::Duration::from_millis(
                    (*duration_ms).min(MAX_ACTION_DURATION_MS),
                ));
                for key in resolved.iter().rev() {
                    enigo
                        .key(*key, Direction::Release)
                        .map_err(|err| format!("key release: {err}"))?;
                }
                Ok(())
            }
            ComputerAction::Wait { duration_ms } => {
                std::thread::sleep(std::time::Duration::from_millis(
                    (*duration_ms).min(MAX_ACTION_DURATION_MS),
                ));
                Ok(())
            }
        }
    }
}

fn to_button(button: MouseButton) -> Button {
    match button {
        MouseButton::Left => Button::Left,
        MouseButton::Right => Button::Right,
        MouseButton::Middle => Button::Middle,
    }
}

/// Press the modifier keys named in `modifiers`, run `body`, then release them
/// in reverse order. Unknown modifier names abort with an error.
/// The lowercase key names for a set of typed modifiers, as `with_modifiers`
/// (shared with the string-chord path) expects.
fn modifier_key_names(modifiers: &[Modifier]) -> Vec<String> {
    modifiers
        .iter()
        .map(|modifier| modifier.as_key_name().to_string())
        .collect()
}

fn with_modifiers(
    enigo: &mut Enigo,
    modifiers: &[String],
    body: impl FnOnce(&mut Enigo) -> Result<(), String>,
) -> Result<(), String> {
    let keys: Vec<Key> = modifiers
        .iter()
        .map(|m| {
            parse_key(&m.to_ascii_lowercase()).ok_or_else(|| format!("unknown modifier '{m}'"))
        })
        .collect::<Result<_, _>>()?;
    for key in &keys {
        enigo
            .key(*key, Direction::Press)
            .map_err(|err| format!("modifier press: {err}"))?;
    }
    let result = body(enigo);
    for key in keys.iter().rev() {
        // Always attempt to release, even if the body failed, so we never leak
        // a stuck modifier.
        let _ = enigo.key(*key, Direction::Release);
    }
    result
}

/// Press a `+`-separated chord: hold all-but-last as modifiers, click the last.
/// A single key (no `+`) is just clicked.
fn press_chord(enigo: &mut Enigo, chord: &str) -> Result<(), String> {
    let parts = split_chord(chord);
    let Some((last, modifiers)) = parts.split_last() else {
        return Ok(());
    };
    let owned: Vec<String> = modifiers.to_vec();
    with_modifiers(enigo, &owned, |enigo| {
        let key = parse_key(last).ok_or_else(|| format!("unknown key '{last}'"))?;
        enigo
            .key(key, Direction::Click)
            .map_err(|err| format!("key click: {err}"))
    })
}

/// Map a normalized key name to an `enigo::Key`. Single characters fall through
/// to `Key::Unicode`.
fn parse_key(name: &str) -> Option<Key> {
    let key = match name {
        "ctrl" | "control" => Key::Control,
        "shift" => Key::Shift,
        "alt" | "option" => Key::Alt,
        "super" | "cmd" | "command" | "meta" | "win" | "windows" => Key::Meta,
        "return" | "enter" => Key::Return,
        "tab" => Key::Tab,
        "space" => Key::Space,
        "backspace" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "escape" | "esc" => Key::Escape,
        "up" => Key::UpArrow,
        "down" => Key::DownArrow,
        "left" => Key::LeftArrow,
        "right" => Key::RightArrow,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" | "page_up" => Key::PageUp,
        "pagedown" | "page_down" => Key::PageDown,
        "f1" => Key::F1,
        "f2" => Key::F2,
        "f3" => Key::F3,
        "f4" => Key::F4,
        "f5" => Key::F5,
        "f6" => Key::F6,
        "f7" => Key::F7,
        "f8" => Key::F8,
        "f9" => Key::F9,
        "f10" => Key::F10,
        "f11" => Key::F11,
        "f12" => Key::F12,
        other => {
            let mut chars = other.chars();
            let first = chars.next()?;
            if chars.next().is_none() {
                Key::Unicode(first)
            } else {
                return None;
            }
        }
    };
    Some(key)
}

/// Per-OS permission preflight + guidance.
fn platform_permissions() -> PermissionStatus {
    #[cfg(target_os = "macos")]
    {
        let screen = if macos::has_screen_capture_access() {
            PermissionState::Granted
        } else {
            PermissionState::Undetermined
        };
        let trusted = macos::is_process_trusted();
        let input = if trusted {
            PermissionState::Granted
        } else {
            PermissionState::Undetermined
        };
        PermissionStatus {
            screen,
            input,
            accessibility: input,
            os: "macos".to_string(),
            guidance: "Grant this app under System Settings → Privacy & Security → Screen \
                       Recording and Accessibility, then restart it."
                .to_string(),
        }
    }
    #[cfg(target_os = "linux")]
    {
        // X11 needs no grant; Wayland requires a per-session portal consent
        // that we cannot preflight synchronously.
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        let state = if wayland {
            PermissionState::Undetermined
        } else {
            PermissionState::NotRequired
        };
        PermissionStatus {
            screen: state,
            input: state,
            accessibility: PermissionState::Unknown,
            os: "linux".to_string(),
            guidance: if wayland {
                "On Wayland, approve the screen-share / remote-desktop portal dialog when prompted."
                    .to_string()
            } else {
                "X11: no additional permission required.".to_string()
            },
        }
    }
    #[cfg(target_os = "windows")]
    {
        PermissionStatus {
            screen: PermissionState::NotRequired,
            input: PermissionState::NotRequired,
            accessibility: PermissionState::Unknown,
            os: "windows".to_string(),
            guidance: "No additional permission is required for screen capture or input on \
                       Windows."
                .to_string(),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        PermissionStatus {
            screen: PermissionState::Unknown,
            input: PermissionState::Unknown,
            accessibility: PermissionState::Unknown,
            os: std::env::consts::OS.to_string(),
            guidance: "Computer-use permission status is unknown on this platform.".to_string(),
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    // TCC preflight. `CGPreflightScreenCaptureAccess` (CoreGraphics) reports
    // whether Screen Recording is granted without prompting;
    // `AXIsProcessTrusted` (ApplicationServices) reports Accessibility trust,
    // which also gates synthetic `CGEvent` input.
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
    }
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> u8;
    }

    pub(super) fn has_screen_capture_access() -> bool {
        unsafe { CGPreflightScreenCaptureAccess() }
    }

    pub(super) fn is_process_trusted() -> bool {
        unsafe { AXIsProcessTrusted() != 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_key_handles_named_and_unicode() {
        assert!(matches!(parse_key("ctrl"), Some(Key::Control)));
        assert!(matches!(parse_key("return"), Some(Key::Return)));
        assert!(matches!(parse_key("a"), Some(Key::Unicode('a'))));
        assert!(parse_key("notakey").is_none());
    }

    #[test]
    fn capabilities_reports_local() {
        let backend = LocalBackend::new();
        let caps = backend.capabilities();
        assert_eq!(caps.name, "local");
        assert!(caps.screenshot && caps.input);
    }
}
