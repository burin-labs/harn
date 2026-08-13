use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use url::Url;

use super::OAUTH_CALLBACK_TIMEOUT;

const CALLBACK_ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub(super) struct OAuthCallbackResponse {
    pub(super) code: String,
    pub(super) issuer: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum OAuthCallbackError {
    TimedOut,
    UserDenied,
    StateMismatch,
    Invalid(String),
}

impl std::fmt::Display for OAuthCallbackError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimedOut => formatter.write_str("OAuth callback timed out after 5 minutes"),
            Self::UserDenied => formatter.write_str("OAuth authorization was denied"),
            Self::StateMismatch => formatter.write_str("OAuth callback state did not match"),
            Self::Invalid(detail) => formatter.write_str(detail),
        }
    }
}

pub(super) fn bind_loopback_listener(redirect_uri: &str) -> Result<(TcpListener, String), String> {
    let mut parsed =
        Url::parse(redirect_uri).map_err(|error| format!("Invalid redirect URI: {error}"))?;
    if parsed.scheme() != "http" {
        return Err("Redirect URI must use the http scheme for the loopback listener".to_string());
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "Redirect URI must include a host".to_string())?;
    if host != "127.0.0.1" && host != "localhost" {
        return Err("Redirect URI must bind to 127.0.0.1 or localhost".to_string());
    }
    let port = parsed
        .port()
        .ok_or_else(|| "Redirect URI must include a port".to_string())?;
    let listener = TcpListener::bind((host, port))
        .map_err(|error| format!("Failed to bind redirect URI {redirect_uri}: {error}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("Failed to configure redirect listener: {error}"))?;
    let actual_port = listener
        .local_addr()
        .map_err(|error| format!("Failed to inspect redirect listener: {error}"))?
        .port();
    parsed
        .set_port(Some(actual_port))
        .map_err(|_| "failed to render redirect listener port".to_string())?;
    Ok((listener, parsed.to_string()))
}

pub(super) fn wait_for_oauth_response(
    listener: TcpListener,
    redirect_uri: &str,
    expected_state: &str,
) -> Result<OAuthCallbackResponse, OAuthCallbackError> {
    let query = wait_for_callback_query_inner(listener, redirect_uri, Some(expected_state), true)?;
    let code = query
        .iter()
        .find(|(key, _)| key == "code")
        .map(|(_, value)| value.clone())
        .ok_or_else(|| {
            OAuthCallbackError::Invalid(
                "OAuth callback did not include an authorization code".to_string(),
            )
        })?;
    let issuer = query
        .into_iter()
        .find(|(key, _)| key == "iss")
        .map(|(_, value)| value);
    Ok(OAuthCallbackResponse { code, issuer })
}

pub(super) fn wait_for_github_installation(
    listener: TcpListener,
    redirect_uri: &str,
    expected_state: Option<&str>,
) -> Result<String, String> {
    let query = wait_for_callback_query(listener, redirect_uri, expected_state)?;
    query
        .into_iter()
        .find(|(key, _)| key == "installation_id")
        .map(|(_, value)| value)
        .ok_or_else(|| "GitHub callback did not include installation_id".to_string())
}

pub(super) fn wait_for_callback_query(
    listener: TcpListener,
    redirect_uri: &str,
    expected_state: Option<&str>,
) -> Result<Vec<(String, String)>, String> {
    wait_for_callback_query_inner(listener, redirect_uri, expected_state, false)
        .map_err(|error| error.to_string())
}

fn wait_for_callback_query_inner(
    listener: TcpListener,
    redirect_uri: &str,
    expected_state: Option<&str>,
    terminal_oauth_errors: bool,
) -> Result<Vec<(String, String)>, OAuthCallbackError> {
    let parsed_redirect = Url::parse(redirect_uri)
        .map_err(|error| OAuthCallbackError::Invalid(format!("Invalid redirect URI: {error}")))?;
    let expected_path = parsed_redirect.path().to_string();
    let expected_origin = loopback_origin(&parsed_redirect).map_err(OAuthCallbackError::Invalid)?;
    let deadline = Instant::now() + OAUTH_CALLBACK_TIMEOUT;

    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let mut buffer = [0u8; 8192];
                let bytes_read = stream.read(&mut buffer).map_err(|error| {
                    OAuthCallbackError::Invalid(format!("Failed to read OAuth callback: {error}"))
                })?;
                let request = String::from_utf8_lossy(&buffer[..bytes_read]);
                let response;
                let result = parse_callback_request_typed(
                    &request,
                    &expected_path,
                    expected_state,
                    &expected_origin,
                );
                match result {
                    Ok(query) => {
                        response = html_response(
                            200,
                            "Authorization complete. You can close this window.",
                        );
                        let _ = stream.write_all(response.as_bytes());
                        return Ok(query);
                    }
                    Err(error) => {
                        response = html_response(400, &error.to_string());
                        let _ = stream.write_all(response.as_bytes());
                        // A matching `access_denied` response is an authenticated
                        // terminal user choice. A mismatched state is not: any
                        // local process could race the real browser callback, so
                        // RFC 9700 CSRF protection requires us to ignore it and
                        // keep waiting for the transaction-bound response.
                        if terminal_oauth_errors && matches!(error, OAuthCallbackError::UserDenied)
                        {
                            return Err(error);
                        }
                        continue;
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(OAuthCallbackError::TimedOut);
                }
                std::thread::sleep(CALLBACK_ACCEPT_POLL_INTERVAL);
            }
            Err(error) => {
                return Err(OAuthCallbackError::Invalid(format!(
                    "Failed to accept OAuth callback: {error}"
                )))
            }
        }
    }
}

#[cfg(test)]
pub(super) fn parse_callback_request(
    request: &str,
    expected_path: &str,
    expected_state: Option<&str>,
    expected_origin: &str,
) -> Result<Vec<(String, String)>, String> {
    parse_callback_request_typed(request, expected_path, expected_state, expected_origin)
        .map_err(|error| error.to_string())
}

fn parse_callback_request_typed(
    request: &str,
    expected_path: &str,
    expected_state: Option<&str>,
    expected_origin: &str,
) -> Result<Vec<(String, String)>, OAuthCallbackError> {
    let mut lines = request.lines();
    let request_line = lines.next().ok_or_else(|| {
        OAuthCallbackError::Invalid("OAuth callback request was empty".to_string())
    })?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().ok_or_else(|| {
        OAuthCallbackError::Invalid("OAuth callback request line was invalid".to_string())
    })?;
    if method != "GET" {
        return Err(OAuthCallbackError::Invalid(
            "OAuth callback request must use GET".to_string(),
        ));
    }
    let path_and_query = request_parts.next().ok_or_else(|| {
        OAuthCallbackError::Invalid("OAuth callback request line was invalid".to_string())
    })?;
    let version = request_parts.next().ok_or_else(|| {
        OAuthCallbackError::Invalid("OAuth callback request line was invalid".to_string())
    })?;
    if !version.starts_with("HTTP/") || request_parts.next().is_some() {
        return Err(OAuthCallbackError::Invalid(
            "OAuth callback request line was invalid".to_string(),
        ));
    }
    let origin = lines.find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("origin")
            .then(|| value.trim().to_string())
    });
    if let Some(origin) = origin {
        if origin != expected_origin && origin != "null" {
            return Err(OAuthCallbackError::Invalid(
                "OAuth callback Origin header did not match the redirect URI".to_string(),
            ));
        }
    }

    let callback_url =
        Url::parse(&format!("{expected_origin}{path_and_query}")).map_err(|error| {
            OAuthCallbackError::Invalid(format!("OAuth callback URL was invalid: {error}"))
        })?;
    if callback_url.path() != expected_path {
        return Err(OAuthCallbackError::Invalid(
            "Invalid callback path".to_string(),
        ));
    }

    let query = callback_url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    if let Some(expected_state) = expected_state {
        let actual_state = query
            .iter()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.as_str());
        if actual_state != Some(expected_state) {
            return Err(OAuthCallbackError::StateMismatch);
        }
    }
    if let Some((_, error)) = query.iter().find(|(key, _)| key == "error") {
        if error == "access_denied" {
            return Err(OAuthCallbackError::UserDenied);
        }
        return Err(OAuthCallbackError::Invalid(
            "OAuth authorization failed".to_string(),
        ));
    }
    Ok(query)
}

pub(super) fn loopback_origin(url: &Url) -> Result<String, String> {
    let host = url
        .host_str()
        .ok_or_else(|| "Redirect URI must include a host".to_string())?;
    let port = url
        .port()
        .ok_or_else(|| "Redirect URI must include a port".to_string())?;
    Ok(format!("{}://{}:{}", url.scheme(), host, port))
}

pub(super) fn html_response(status: u16, message: &str) -> String {
    let status_line = match status {
        200 => "HTTP/1.1 200 OK",
        400 => "HTTP/1.1 400 Bad Request",
        _ => "HTTP/1.1 404 Not Found",
    };
    let title = if status == 200 {
        "Authorization Complete"
    } else {
        "Authorization Failed"
    };
    let escaped_message = crate::format::escape_html(message);
    format!(
        "{status_line}\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title></head><body><h1>{title}</h1><p>{escaped_message}</p></body></html>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;
    use std::thread;

    #[test]
    fn matching_user_denial_is_terminal_without_waiting_for_timeout() {
        let (listener, redirect_uri) =
            bind_loopback_listener("http://127.0.0.1:0/oauth/callback").unwrap();
        let parsed = Url::parse(&redirect_uri).unwrap();
        let address = format!("127.0.0.1:{}", parsed.port().unwrap());
        let waiter = thread::spawn(move || {
            wait_for_oauth_response(listener, &redirect_uri, "expected-state")
        });

        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .write_all(
                b"GET /oauth/callback?error=access_denied&state=expected-state HTTP/1.1\r\n\r\n",
            )
            .unwrap();

        assert!(matches!(
            waiter.join().unwrap(),
            Err(OAuthCallbackError::UserDenied)
        ));
    }
}
