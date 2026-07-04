//! Non-local computer-use backends.
//!
//! - [`NullBackend`] is the graceful-degradation backend: every call fails
//!   with a stable, explanatory message so an un-provisioned environment (no
//!   `computer-local` feature, transport disabled, missing endpoint) never
//!   panics and always tells the operator why.
//! - [`SocketBackend`] is a line-delimited JSON-RPC client over a Unix socket.
//!   It is the transport the macOS non-sandboxed helper and (later) the cloud
//!   desktop sandbox speak, so the sandboxed GUI and remote runners reach the
//!   exact same [`ComputerBackend`] contract as an in-process local run.
//!
//! Wire protocol (one JSON object per line, request then response):
//! - request:  `{"op":"screenshot"}` / `{"op":"execute","actions":[...]}` /
//!   `{"op":"ui_tree"}` / `{"op":"permissions"}`
//! - response: `{"ok":true,"result":<value>}` or `{"ok":false,"error":"..."}`

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
struct WireResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    result: Option<serde_json::Value>,
}

/// JSON-RPC-over-Unix-socket client backend (helper / remote transports).
pub struct SocketBackend {
    endpoint: SocketEndpoint,
}

enum SocketEndpoint {
    #[cfg(unix)]
    Unix(std::path::PathBuf),
}

impl SocketBackend {
    /// Build from `BURIN_COMPUTER_USE_ENDPOINT`. `transport` is only used for
    /// error messages (`helper` vs `remote`). Supported forms:
    /// - `unix:/absolute/path.sock`
    /// - a bare absolute path (treated as a Unix socket)
    pub fn from_env(transport: &str) -> Result<Self, String> {
        let raw = std::env::var("BURIN_COMPUTER_USE_ENDPOINT").map_err(|_| {
            format!(
                "computer-use transport '{transport}' requires BURIN_COMPUTER_USE_ENDPOINT \
                 (e.g. unix:/path/to/socket)"
            )
        })?;
        Self::from_endpoint(&raw)
    }

    /// Build from an explicit endpoint string.
    pub fn from_endpoint(raw: &str) -> Result<Self, String> {
        let path = raw.strip_prefix("unix:").unwrap_or(raw);
        if let Some(rest) = raw.strip_prefix("tcp:") {
            return Err(format!(
                "tcp computer-use endpoints are not supported yet (got 'tcp:{rest}')"
            ));
        }
        #[cfg(unix)]
        {
            Ok(Self {
                endpoint: SocketEndpoint::Unix(std::path::PathBuf::from(path)),
            })
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Err("socket computer-use transport is only available on Unix platforms".to_string())
        }
    }

    fn call(&self, request: &WireRequest<'_>) -> Result<Option<serde_json::Value>, String> {
        #[cfg(unix)]
        {
            use std::io::{BufRead, BufReader, Write};
            use std::os::unix::net::UnixStream;

            let SocketEndpoint::Unix(path) = &self.endpoint;
            let mut stream = UnixStream::connect(path)
                .map_err(|err| format!("connect {}: {err}", path.display()))?;
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
        #[cfg(not(unix))]
        {
            let _ = request;
            Err("socket computer-use transport is only available on Unix platforms".to_string())
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

impl ComputerBackend for SocketBackend {
    fn capabilities(&self) -> BackendCapabilities {
        // The remote peer decides what it truly supports; advertise the full
        // surface optimistically and let individual calls report failures.
        BackendCapabilities {
            name: "socket".to_string(),
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
