//! In-process local backend: real screen capture (`xcap`) and synthetic input
//! (`enigo`), plus per-OS permission preflight. Compiled only under the
//! `computer-local` Cargo feature so headless clients never pull the OS
//! capture/input toolchains.

use std::io::Cursor;
use std::sync::Mutex;

use base64::Engine as _;
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard as _, Mouse as _, Settings};

use super::{
    split_chord, BackendCapabilities, ComputerAction, ComputerBackend, MouseButton,
    PermissionState, PermissionStatus, ScreenImage, ScrollDirection, UiTree,
};

/// Local capture/input backend. Cheap to construct; captures a fresh `Enigo`
/// per action batch and enumerates monitors per screenshot.
pub struct LocalBackend {
    /// Backing-scale factor of the last captured display. Screenshots are
    /// physical pixels but some platforms (macOS) take synthetic input in
    /// logical points, so we divide incoming coordinates by this. The agent
    /// loop always screenshots before acting, so the cached value is fresh;
    /// defaults to 1.0.
    last_scale: Mutex<f64>,
}

impl LocalBackend {
    /// Construct a local backend.
    pub fn new() -> Self {
        Self {
            last_scale: Mutex::new(1.0),
        }
    }

    /// Map an action coordinate (physical pixels, screenshot space) into the
    /// coordinate space `enigo` expects for this platform.
    fn to_input_coords(&self, x: i32, y: i32) -> (i32, i32) {
        if cfg!(target_os = "macos") {
            let scale = *self.last_scale.lock().expect("scale mutex");
            if scale > 0.0 {
                return (
                    (f64::from(x) / scale).round() as i32,
                    (f64::from(y) / scale).round() as i32,
                );
            }
        }
        (x, y)
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

        let monitors = Monitor::all().map_err(|err| format!("enumerate monitors: {err}"))?;
        let monitor = monitors
            .into_iter()
            .find(|m| m.is_primary().unwrap_or(false))
            .or_else(|| Monitor::all().ok().and_then(|mut m| m.drain(..).next()))
            .ok_or_else(|| "no monitor found".to_string())?;

        let image = monitor
            .capture_image()
            .map_err(|err| format!("capture screen: {err}"))?;
        let (width, height) = (image.width(), image.height());

        // scale_factor = captured (physical) width / reported (logical) width.
        let logical_width = monitor.width().unwrap_or(width).max(1);
        let scale_factor = f64::from(width) / f64::from(logical_width);
        *self.last_scale.lock().expect("scale mutex") = scale_factor;

        let mut png = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|err| format!("encode png: {err}"))?;
        let base64 = base64::engine::general_purpose::STANDARD.encode(&png);

        Ok(ScreenImage {
            base64,
            media_type: "image/png".to_string(),
            width,
            height,
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
                let (x, y) = self.to_input_coords(*x, *y);
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
                let (x, y) = self.to_input_coords(*x, *y);
                enigo
                    .move_mouse(x, y, Coordinate::Abs)
                    .map_err(|err| format!("move_mouse: {err}"))?;
                with_modifiers(enigo, modifiers, |enigo| {
                    for _ in 0..(*count).max(1) {
                        enigo
                            .button(to_button(*button), Direction::Click)
                            .map_err(|err| format!("button click: {err}"))?;
                    }
                    Ok(())
                })
            }
            ComputerAction::MouseDown { button, x, y } => {
                let (x, y) = self.to_input_coords(*x, *y);
                enigo
                    .move_mouse(x, y, Coordinate::Abs)
                    .map_err(|err| format!("move_mouse: {err}"))?;
                enigo
                    .button(to_button(*button), Direction::Press)
                    .map_err(|err| format!("button press: {err}"))
            }
            ComputerAction::MouseUp { button, x, y } => {
                let (x, y) = self.to_input_coords(*x, *y);
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
                let (fx, fy) = self.to_input_coords(*from_x, *from_y);
                let (tx, ty) = self.to_input_coords(*to_x, *to_y);
                with_modifiers(enigo, modifiers, |enigo| {
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
                let (x, y) = self.to_input_coords(*x, *y);
                enigo
                    .move_mouse(x, y, Coordinate::Abs)
                    .map_err(|err| format!("move_mouse: {err}"))?;
                let (axis, magnitude) = match direction {
                    ScrollDirection::Down => (Axis::Vertical, *amount),
                    ScrollDirection::Up => (Axis::Vertical, -*amount),
                    ScrollDirection::Right => (Axis::Horizontal, *amount),
                    ScrollDirection::Left => (Axis::Horizontal, -*amount),
                };
                with_modifiers(enigo, modifiers, |enigo| {
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
                std::thread::sleep(std::time::Duration::from_millis(*duration_ms));
                for key in resolved.iter().rev() {
                    enigo
                        .key(*key, Direction::Release)
                        .map_err(|err| format!("key release: {err}"))?;
                }
                Ok(())
            }
            ComputerAction::Wait { duration_ms } => {
                std::thread::sleep(std::time::Duration::from_millis(*duration_ms));
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
