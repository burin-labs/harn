use std::time::Duration;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const DEFAULT_TIMEOUT_MS: u64 = 1_000;
const DEFAULT_MAX_RESPONSE_BYTES: usize = 1_048_576;

pub(crate) fn register_net_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[&NET_UNIX_SOCKET_JSON_REQUEST_IMPL_DEF];

#[derive(Clone, Copy)]
struct UnixSocketOptions {
    timeout: Duration,
    max_response_bytes: usize,
}

fn option_int(options: Option<&crate::value::DictMap>, key: &str, default: i64) -> i64 {
    options
        .and_then(|opts| opts.get(key))
        .and_then(VmValue::as_int)
        .unwrap_or(default)
}

fn unix_socket_options(value: Option<&VmValue>) -> UnixSocketOptions {
    let options = value.and_then(VmValue::as_dict);
    let timeout_ms = option_int(options, "timeout_ms", DEFAULT_TIMEOUT_MS as i64).max(1) as u64;
    let max_response_bytes = option_int(
        options,
        "max_response_bytes",
        DEFAULT_MAX_RESPONSE_BYTES as i64,
    )
    .max(1) as usize;
    UnixSocketOptions {
        timeout: Duration::from_millis(timeout_ms),
        max_response_bytes,
    }
}

fn insert_string(dict: &mut crate::value::DictMap, key: &str, value: impl Into<String>) {
    dict.insert(
        crate::value::intern_key(key),
        VmValue::String(arcstr::ArcStr::from(value.into())),
    );
}

fn insert_int(dict: &mut crate::value::DictMap, key: &str, value: i64) {
    dict.insert(crate::value::intern_key(key), VmValue::Int(value));
}

fn elapsed_ms(started_ms: i64) -> i64 {
    crate::stdlib::clock::now_monotonic_ms().saturating_sub(started_ms)
}

fn socket_result(
    started_ms: i64,
    path: &str,
    ok: bool,
    status: &str,
    error: Option<String>,
    raw_response: Option<String>,
    response: Option<VmValue>,
) -> VmValue {
    let mut out = crate::value::DictMap::new();
    out.insert(crate::value::intern_key("ok"), VmValue::Bool(ok));
    insert_string(&mut out, "status", status);
    insert_string(&mut out, "path", path);
    insert_int(&mut out, "duration_ms", elapsed_ms(started_ms));
    if let Some(error) = error {
        insert_string(&mut out, "error", error);
    }
    if let Some(raw_response) = raw_response {
        insert_int(&mut out, "bytes_read", raw_response.len() as i64);
        insert_string(&mut out, "raw_response", raw_response);
    }
    if let Some(response) = response {
        out.insert(crate::value::intern_key("response"), response);
    }
    VmValue::dict(out)
}

#[cfg(unix)]
fn io_status(error: &std::io::Error) -> &'static str {
    use std::io;

    match error.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => "timeout",
        _ => "io_error",
    }
}

#[cfg(unix)]
fn unix_socket_json_request(
    path: &str,
    request_json: &str,
    options: UnixSocketOptions,
    started_ms: i64,
) -> VmValue {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::net::UnixStream;

    let mut stream = match UnixStream::connect(path) {
        Ok(stream) => stream,
        Err(error) => {
            return socket_result(
                started_ms,
                path,
                false,
                "connect_error",
                Some(error.to_string()),
                None,
                None,
            );
        }
    };
    let _ = stream.set_read_timeout(Some(options.timeout));
    let _ = stream.set_write_timeout(Some(options.timeout));

    if let Err(error) = stream.write_all(request_json.as_bytes()) {
        return socket_result(
            started_ms,
            path,
            false,
            io_status(&error),
            Some(error.to_string()),
            None,
            None,
        );
    }
    if let Err(error) = stream.write_all(b"\n") {
        return socket_result(
            started_ms,
            path,
            false,
            io_status(&error),
            Some(error.to_string()),
            None,
            None,
        );
    }
    if let Err(error) = stream.flush() {
        return socket_result(
            started_ms,
            path,
            false,
            io_status(&error),
            Some(error.to_string()),
            None,
            None,
        );
    }

    let mut reader = BufReader::new(stream).take((options.max_response_bytes as u64) + 1);
    let mut response_bytes = Vec::new();
    match reader.read_until(b'\n', &mut response_bytes) {
        Ok(0) => {
            return socket_result(
                started_ms,
                path,
                false,
                "eof",
                Some("socket closed before a response line was received".to_string()),
                None,
                None,
            );
        }
        Ok(_) => {}
        Err(error) => {
            return socket_result(
                started_ms,
                path,
                false,
                io_status(&error),
                Some(error.to_string()),
                None,
                None,
            );
        }
    }
    if response_bytes.len() > options.max_response_bytes {
        return socket_result(
            started_ms,
            path,
            false,
            "response_too_large",
            Some(format!(
                "response exceeded max_response_bytes={}",
                options.max_response_bytes
            )),
            None,
            None,
        );
    }

    while matches!(response_bytes.last(), Some(b'\n' | b'\r')) {
        response_bytes.pop();
    }
    let raw_response = match String::from_utf8(response_bytes) {
        Ok(text) => text,
        Err(error) => {
            return socket_result(
                started_ms,
                path,
                false,
                "invalid_utf8",
                Some(error.to_string()),
                None,
                None,
            );
        }
    };
    let parsed = match serde_json::from_str::<serde_json::Value>(&raw_response) {
        Ok(value) => crate::schema::json_to_vm_value(&value),
        Err(error) => {
            return socket_result(
                started_ms,
                path,
                false,
                "invalid_json",
                Some(error.to_string()),
                Some(raw_response),
                None,
            );
        }
    };
    socket_result(
        started_ms,
        path,
        true,
        "ok",
        None,
        Some(raw_response),
        Some(parsed),
    )
}

#[cfg(not(unix))]
fn unix_socket_json_request(
    path: &str,
    _request_json: &str,
    options: UnixSocketOptions,
    started_ms: i64,
) -> VmValue {
    let _ = (options.timeout, options.max_response_bytes);
    socket_result(
        started_ms,
        path,
        false,
        "unsupported",
        Some("Unix domain sockets are not supported on this target".to_string()),
        None,
        None,
    )
}

#[harn_builtin(
    sig = "__net_unix_socket_json_request(path: string, request: any, options?: dict) -> dict",
    category = "net"
)]
fn net_unix_socket_json_request_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let started_ms = crate::stdlib::clock::now_monotonic_ms();
    let path = args.first().map(VmValue::display).unwrap_or_default();
    if path.is_empty() {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "__net_unix_socket_json_request: path is required",
        ))));
    }
    let request = args.get(1).unwrap_or(&VmValue::Nil);
    let request_json = super::json::vm_value_to_json(request);
    let options = unix_socket_options(args.get(2));
    Ok(unix_socket_json_request(
        &path,
        &request_json,
        options,
        started_ms,
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::Shutdown;

    use super::*;

    #[test]
    fn unix_socket_json_request_round_trips_json_line() {
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().expect("tempdir");
        let socket_path = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind unix listener");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = String::new();
            BufReader::new(stream.try_clone().expect("clone stream"))
                .read_line(&mut request)
                .expect("read request");
            let parsed: serde_json::Value = serde_json::from_str(request.trim()).expect("json");
            assert_eq!(parsed["method"], "ping");
            stream
                .write_all(br#"{"ok":true,"method":"pong"}"#)
                .expect("write response");
            stream.write_all(b"\n").expect("write newline");
        });

        let result = unix_socket_json_request(
            &socket_path.display().to_string(),
            r#"{"method":"ping"}"#,
            UnixSocketOptions {
                timeout: Duration::from_millis(500),
                max_response_bytes: 1024,
            },
            crate::stdlib::clock::now_monotonic_ms(),
        );
        server.join().expect("server thread");

        let dict = result.as_dict().expect("result dict");
        assert!(matches!(dict.get("ok"), Some(VmValue::Bool(true))));
        assert_eq!(
            dict.get("status").map(VmValue::display).as_deref(),
            Some("ok")
        );
        let response = dict
            .get("response")
            .and_then(VmValue::as_dict)
            .expect("response dict");
        assert_eq!(
            response.get("method").map(VmValue::display).as_deref(),
            Some("pong")
        );
    }

    #[test]
    fn unix_socket_json_request_reports_invalid_json() {
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().expect("tempdir");
        let socket_path = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind unix listener");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = String::new();
            BufReader::new(stream.try_clone().expect("clone stream"))
                .read_line(&mut request)
                .expect("read request");
            stream.write_all(b"not-json\n").expect("write response");
            stream.flush().expect("flush response");
            stream
                .shutdown(Shutdown::Write)
                .expect("shutdown write half");
        });

        let result = unix_socket_json_request(
            &socket_path.display().to_string(),
            r#"{"method":"ping"}"#,
            UnixSocketOptions {
                timeout: Duration::from_millis(500),
                max_response_bytes: 1024,
            },
            crate::stdlib::clock::now_monotonic_ms(),
        );
        server.join().expect("server thread");

        let dict = result.as_dict().expect("result dict");
        assert!(matches!(dict.get("ok"), Some(VmValue::Bool(false))));
        assert_eq!(
            dict.get("status").map(VmValue::display).as_deref(),
            Some("invalid_json")
        );
        assert_eq!(
            dict.get("raw_response").map(VmValue::display).as_deref(),
            Some("not-json")
        );
    }
}
