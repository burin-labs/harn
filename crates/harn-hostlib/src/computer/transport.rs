//! Non-local computer-use backends and the reusable socket wire protocol.
//!
//! - [`NullBackend`] is the graceful-degradation backend: every call fails
//!   with a stable, explanatory message so an un-provisioned environment (no
//!   `computer-local` feature, transport disabled, missing endpoint) never
//!   panics and always tells the operator why.
//! - [`SocketBackend`] is a line-delimited JSON-RPC *client* over a Unix
//!   socket (the macOS non-sandboxed helper) or TCP (a cloud desktop sandbox),
//!   so the sandboxed GUI and remote runners reach the exact same
//!   [`ComputerBackend`] contract as an in-process local run.
//! - [`handle_request_line`] is the reusable *server* half: it decodes one
//!   request line, invokes a backend, and returns the response line. The macOS
//!   helper and the cloud desktop runner both wrap it around their accept loop,
//!   so the wire protocol lives in exactly one place.
//!
//! Wire protocol (one JSON object per line, request then response):
//! - request:  `{"op":"screenshot"}` / `{"op":"execute","actions":[...]}` /
//!   `{"op":"ui_tree"}` / `{"op":"permissions"}`
//! - response: `{"ok":true,"result":<value>}` or `{"ok":false,"error":"..."}`

use std::io::{BufRead, BufReader, Read, Write};

use serde::{Deserialize, Serialize};

use super::{
    BackendCapabilities, ComputerAction, ComputerBackend, PermissionState, PermissionStatus,
    ScreenImage, UiTree,
};

/// Backend that fails every operation with a fixed message.
pub struct NullBackend {
    message: String,
}

impl NullBackend {
    /// Construct with the explanation returned by every failing call.
    pub fn new(message: String) -> Self {
        Self { message }
    }
}

impl ComputerBackend for NullBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            name: "null".to_string(),
            screenshot: false,
            input: false,
            ui_tree: false,
        }
    }

    fn screenshot(&self) -> Result<ScreenImage, String> {
        Err(self.message.clone())
    }

    fn execute(&self, _actions: &[ComputerAction]) -> Result<(), String> {
        Err(self.message.clone())
    }

    fn ui_tree(&self) -> Result<UiTree, String> {
        Err(self.message.clone())
    }

    fn permissions(&self) -> Result<PermissionStatus, String> {
        // Report an honest "unknown, and here's why" rather than erroring, so
        // the permission-status surface can always render a guidance string.
        Ok(PermissionStatus {
            screen: PermissionState::Unknown,
            input: PermissionState::Unknown,
            accessibility: PermissionState::Unknown,
            os: std::env::consts::OS.to_string(),
            guidance: self.message.clone(),
        })
    }
}

#[derive(Serialize)]
struct WireRequest<'a> {
    op: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    actions: Option<&'a [ComputerAction]>,
}

#[derive(Deserialize)]
struct ServerRequest {
    op: String,
    #[serde(default)]
    actions: Vec<ComputerAction>,
}

#[derive(Serialize, Deserialize)]
struct WireResponse {
    ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
}

impl WireResponse {
    fn ok(result: Option<serde_json::Value>) -> Self {
        Self {
            ok: true,
            error: None,
            result,
        }
    }

    fn err(message: String) -> Self {
        Self {
            ok: false,
            error: Some(message),
            result: None,
        }
    }
}

/// JSON-RPC client backend over a Unix socket (`helper`) or TCP (`remote`).
pub struct SocketBackend {
    endpoint: SocketEndpoint,
}

enum SocketEndpoint {
    #[cfg(unix)]
    Unix(std::path::PathBuf),
    Tcp(String),
}

impl SocketBackend {
    /// Build from `BURIN_COMPUTER_USE_ENDPOINT`. `transport` is only used in
    /// error messages (`helper` vs `remote`). Supported forms:
    /// - `unix:/absolute/path.sock` (or a bare absolute path) — Unix socket
    /// - `tcp:host:port` — TCP (used by the cloud desktop sandbox)
    pub fn from_env(transport: &str) -> Result<Self, String> {
        let raw = std::env::var("BURIN_COMPUTER_USE_ENDPOINT").map_err(|_| {
            format!(
                "computer-use transport '{transport}' requires BURIN_COMPUTER_USE_ENDPOINT \
                 (e.g. unix:/path/to/socket or tcp:host:port)"
            )
        })?;
        Self::from_endpoint(&raw)
    }

    /// Build from an explicit endpoint string.
    pub fn from_endpoint(raw: &str) -> Result<Self, String> {
        if let Some(addr) = raw.strip_prefix("tcp:") {
            if addr.is_empty() {
                return Err("empty tcp computer-use endpoint".to_string());
            }
            return Ok(Self {
                endpoint: SocketEndpoint::Tcp(addr.to_string()),
            });
        }
        let path = raw.strip_prefix("unix:").unwrap_or(raw);
        #[cfg(unix)]
        {
            Ok(Self {
                endpoint: SocketEndpoint::Unix(std::path::PathBuf::from(path)),
            })
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Err(
                "unix computer-use endpoints require a Unix platform; use tcp:host:port"
                    .to_string(),
            )
        }
    }

    fn call(&self, request: &WireRequest<'_>) -> Result<Option<serde_json::Value>, String> {
        match &self.endpoint {
            #[cfg(unix)]
            SocketEndpoint::Unix(path) => {
                let stream = std::os::unix::net::UnixStream::connect(path)
                    .map_err(|err| format!("connect {}: {err}", path.display()))?;
                exchange(stream, request)
            }
            SocketEndpoint::Tcp(addr) => {
                let stream = std::net::TcpStream::connect(addr)
                    .map_err(|err| format!("connect {addr}: {err}"))?;
                exchange(stream, request)
            }
        }
    }

    fn call_typed<T: for<'de> Deserialize<'de>>(
        &self,
        request: &WireRequest<'_>,
    ) -> Result<T, String> {
        let result = self
            .call(request)?
            .ok_or_else(|| "remote computer-use call returned no result".to_string())?;
        serde_json::from_value(result).map_err(|err| format!("decode result: {err}"))
    }
}

/// Write one request line to `stream` and read one response line back.
fn exchange<S: Read + Write>(
    mut stream: S,
    request: &WireRequest<'_>,
) -> Result<Option<serde_json::Value>, String> {
    let mut line =
        serde_json::to_string(request).map_err(|err| format!("encode request: {err}"))?;
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .map_err(|err| format!("write request: {err}"))?;
    stream.flush().map_err(|err| format!("flush: {err}"))?;

    let mut reader = BufReader::new(stream);
    let mut response_line = String::new();
    reader
        .read_line(&mut response_line)
        .map_err(|err| format!("read response: {err}"))?;
    let response: WireResponse = serde_json::from_str(response_line.trim())
        .map_err(|err| format!("decode response: {err}"))?;
    if !response.ok {
        return Err(response
            .error
            .unwrap_or_else(|| "remote computer-use call failed".to_string()));
    }
    Ok(response.result)
}

impl ComputerBackend for SocketBackend {
    fn capabilities(&self) -> BackendCapabilities {
        // The remote peer decides what it truly supports; advertise the full
        // surface optimistically and let individual calls report failures.
        BackendCapabilities {
            // Name the concrete endpoint kind (the `helper`/`remote` transports
            // both resolve to a socket, over a Unix path or TCP respectively).
            name: match &self.endpoint {
                #[cfg(unix)]
                SocketEndpoint::Unix(_) => "socket:unix".to_string(),
                SocketEndpoint::Tcp(_) => "socket:tcp".to_string(),
            },
            screenshot: true,
            input: true,
            ui_tree: true,
        }
    }

    fn screenshot(&self) -> Result<ScreenImage, String> {
        self.call_typed(&WireRequest {
            op: "screenshot",
            actions: None,
        })
    }

    fn execute(&self, actions: &[ComputerAction]) -> Result<(), String> {
        self.call(&WireRequest {
            op: "execute",
            actions: Some(actions),
        })
        .map(|_| ())
    }

    fn ui_tree(&self) -> Result<UiTree, String> {
        self.call_typed(&WireRequest {
            op: "ui_tree",
            actions: None,
        })
    }

    fn permissions(&self) -> Result<PermissionStatus, String> {
        self.call_typed(&WireRequest {
            op: "permissions",
            actions: None,
        })
    }
}

/// Server half of the wire protocol: decode one request line, run it against
/// `backend`, and return the response line (with trailing newline).
///
/// This is the single source of truth for the wire protocol's server side. The
/// macOS helper and the cloud desktop runner wrap it around their own accept
/// loop, so neither re-implements request decoding or response shaping.
///
/// SECURITY: this handler authenticates nothing — any peer that can send a line
/// on the transport gets full desktop control. The CALLER MUST enforce isolation
/// at the transport layer: a filesystem-permission-restricted Unix socket
/// (mode 0600, owner-only) for the local helper, or network isolation + an auth
/// token for a remote/TCP runner. Never expose this server on an unauthenticated
/// network port.
pub fn handle_request_line(backend: &dyn ComputerBackend, line: &str) -> String {
    let response = match serde_json::from_str::<ServerRequest>(line.trim()) {
        Ok(request) => dispatch(backend, &request),
        Err(err) => WireResponse::err(format!("decode request: {err}")),
    };
    let mut encoded = serde_json::to_string(&response)
        .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"encode response failed\"}".to_string());
    encoded.push('\n');
    encoded
}

fn dispatch(backend: &dyn ComputerBackend, request: &ServerRequest) -> WireResponse {
    let outcome: Result<Option<serde_json::Value>, String> = match request.op.as_str() {
        "screenshot" => backend
            .screenshot()
            .and_then(|image| to_value(&image))
            .map(Some),
        "execute" => backend.execute(&request.actions).map(|()| None),
        "ui_tree" => backend.ui_tree().and_then(|tree| to_value(&tree)).map(Some),
        "permissions" => backend
            .permissions()
            .and_then(|status| to_value(&status))
            .map(Some),
        other => Err(format!("unknown op '{other}'")),
    };
    match outcome {
        Ok(result) => WireResponse::ok(result),
        Err(message) => WireResponse::err(message),
    }
}

fn to_value<T: Serialize>(value: &T) -> Result<serde_json::Value, String> {
    serde_json::to_value(value).map_err(|err| format!("encode result: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computer::ScrollDirection;

    struct FakeBackend;

    impl ComputerBackend for FakeBackend {
        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                name: "fake".to_string(),
                screenshot: true,
                input: true,
                ui_tree: false,
            }
        }
        fn screenshot(&self) -> Result<ScreenImage, String> {
            Ok(ScreenImage {
                base64: "AAAA".to_string(),
                media_type: "image/png".to_string(),
                width: 8,
                height: 6,
                scale_factor: 2.0,
            })
        }
        fn execute(&self, actions: &[ComputerAction]) -> Result<(), String> {
            // Echo a marker so the test can assert the actions decoded.
            if actions.len() == 1 {
                Ok(())
            } else {
                Err(format!("expected 1 action, got {}", actions.len()))
            }
        }
        fn ui_tree(&self) -> Result<UiTree, String> {
            Ok(UiTree::default())
        }
        fn permissions(&self) -> Result<PermissionStatus, String> {
            Ok(PermissionStatus {
                screen: PermissionState::Granted,
                input: PermissionState::Granted,
                accessibility: PermissionState::Granted,
                os: "test".to_string(),
                guidance: String::new(),
            })
        }
    }

    #[test]
    fn server_screenshot_roundtrips_through_client_decode() {
        let response = handle_request_line(&FakeBackend, "{\"op\":\"screenshot\"}");
        let parsed: WireResponse = serde_json::from_str(response.trim()).expect("parse");
        assert!(parsed.ok);
        let image: ScreenImage =
            serde_json::from_value(parsed.result.expect("result")).expect("image");
        assert_eq!(image.width, 8);
        assert_eq!(image.scale_factor, 2.0);
    }

    #[test]
    fn server_executes_decoded_actions() {
        let line = "{\"op\":\"execute\",\"actions\":[{\"action\":\"click\",\"x\":1,\"y\":2}]}";
        let response = handle_request_line(&FakeBackend, line);
        let parsed: WireResponse = serde_json::from_str(response.trim()).expect("parse");
        assert!(parsed.ok, "response: {response}");
    }

    #[test]
    fn server_reports_unknown_op() {
        let response = handle_request_line(&FakeBackend, "{\"op\":\"nope\"}");
        let parsed: WireResponse = serde_json::from_str(response.trim()).expect("parse");
        assert!(!parsed.ok);
        assert!(parsed.error.unwrap().contains("unknown op"));
    }

    #[test]
    fn server_reports_bad_json() {
        let response = handle_request_line(&FakeBackend, "not json");
        let parsed: WireResponse = serde_json::from_str(response.trim()).expect("parse");
        assert!(!parsed.ok);
    }

    #[test]
    fn tcp_endpoint_parses() {
        assert!(SocketBackend::from_endpoint("tcp:127.0.0.1:9000").is_ok());
        assert!(SocketBackend::from_endpoint("tcp:").is_err());
    }

    #[test]
    fn wire_request_serializes_actions() {
        let actions = vec![ComputerAction::Scroll {
            x: 1,
            y: 2,
            direction: ScrollDirection::Down,
            amount: 3,
            modifiers: vec![],
        }];
        let request = WireRequest {
            op: "execute",
            actions: Some(&actions),
        };
        let json = serde_json::to_string(&request).expect("encode");
        assert!(json.contains("\"op\":\"execute\""));
        assert!(json.contains("\"action\":\"scroll\""));
        // A no-arg request omits the actions field entirely.
        let bare = serde_json::to_string(&WireRequest {
            op: "screenshot",
            actions: None,
        })
        .expect("encode");
        assert_eq!(bare, "{\"op\":\"screenshot\"}");
    }
}
