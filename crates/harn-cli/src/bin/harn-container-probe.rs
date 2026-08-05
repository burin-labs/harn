use std::env;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::process::ExitCode;
use std::time::Duration;

const DEFAULT_HEALTHCHECK_URL: &str = "http://127.0.0.1:8080/health";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let url = env::var("HARN_CONTAINER_HEALTHCHECK_URL").unwrap_or_else(|_| {
        derive_healthcheck_url().unwrap_or_else(|| DEFAULT_HEALTHCHECK_URL.to_string())
    });
    let Some((authority, request_path)) = parse_http_url(&url) else {
        return Err(format!(
            "unsupported healthcheck URL '{url}'; expected http://host:port/path"
        ));
    };
    let addr = authority
        .to_socket_addrs()
        .map_err(|error| format!("failed to resolve {authority}: {error}"))?
        .next()
        .ok_or_else(|| format!("no socket addresses resolved for {authority}"))?;

    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))
        .map_err(|error| format!("failed to connect to {addr}: {error}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| format!("failed to set read timeout: {error}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| format!("failed to set write timeout: {error}"))?;

    let request =
        format!("GET {request_path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("failed to write request: {error}"))?;

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|error| format!("failed to read response: {error}"))?;

    if response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200") {
        Ok(())
    } else {
        let status_line = response.lines().next().unwrap_or("<empty response>");
        Err(format!("healthcheck failed: {status_line}"))
    }
}

fn derive_healthcheck_url() -> Option<String> {
    let bind = env::var("HARN_ORCHESTRATOR_LISTEN").ok()?;
    let bind = bind.trim();
    if bind.is_empty() {
        return None;
    }
    let port = bind
        .rsplit(':')
        .next()
        .filter(|segment| !segment.is_empty())?;
    Some(format!("http://127.0.0.1:{port}/health"))
}

fn parse_http_url(url: &str) -> Option<(&str, &str)> {
    let remainder = url.strip_prefix("http://")?;
    let (authority, path) = match remainder.find('/') {
        Some(index) => (&remainder[..index], &remainder[index..]),
        None => (remainder, "/"),
    };
    if authority.is_empty() {
        return None;
    }
    Some((authority, path))
}

#[cfg(test)]
mod tests {
    use super::parse_http_url;

    #[test]
    fn parses_http_url_with_default_path() {
        assert_eq!(
            parse_http_url("http://127.0.0.1:8080"),
            Some(("127.0.0.1:8080", "/"))
        );
    }

    #[test]
    fn parses_http_url_with_explicit_path() {
        assert_eq!(
            parse_http_url("http://localhost:9000/health"),
            Some(("localhost:9000", "/health"))
        );
    }
}
