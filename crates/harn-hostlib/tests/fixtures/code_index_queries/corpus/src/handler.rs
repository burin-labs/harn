pub fn handle_request(path: &str) -> String {
    if path == "/health" {
        return "ok".to_string();
    }
    format!("not found: {path}")
}

pub fn handle_error(message: &str) -> String {
    format!("error: {message}")
}
