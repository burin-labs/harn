//! Build-output diagnostic parsers.
//!
//! Each ecosystem emits errors in a different shape. Where possible we
//! consume the runner's machine-readable output (cargo's
//! `--message-format=json`, gocompile's `path:line:col: text` lines).
//! Otherwise we fall back to a generic regex sweep that picks up
//! `path:line:col: error: message` and `error: message` patterns common to
//! C/C++/Swift/TypeScript.

/// Severity of a build diagnostic. Maps 1:1 to the schema enum at
/// `schemas/tools/run_build_command.response.json#/$defs/Diagnostic/properties/severity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Severity {
    Error,
    Warning,
    Note,
    Help,
}

impl Severity {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
            Severity::Help => "help",
        }
    }

    fn parse(token: &str) -> Option<Severity> {
        match token.trim().to_ascii_lowercase().as_str() {
            "error" | "fatal error" | "fatal" => Some(Severity::Error),
            "warning" | "warn" => Some(Severity::Warning),
            "note" | "info" => Some(Severity::Note),
            "help" | "hint" => Some(Severity::Help),
            _ => None,
        }
    }
}

/// One parsed diagnostic. Matches the `Diagnostic` schema in
/// `run_build_command.response.json`.
#[derive(Debug, Clone)]
pub(crate) struct Diagnostic {
    pub(crate) severity: Severity,
    pub(crate) message: String,
    pub(crate) path: Option<String>,
    pub(crate) line: Option<i64>,
    pub(crate) column: Option<i64>,
}

/// Which parser to apply to the captured stdout/stderr. Determined either
/// by the requested ecosystem or — when the caller passed argv — by
/// pattern-matching the first argv element.
#[derive(Debug, Clone, Copy)]
pub(crate) enum DiagnosticSource {
    /// Cargo with `--message-format=json` (one JSON object per line).
    CargoJson,
    /// `go build` / `go vet` `path:line: text` style output.
    GoBuild,
    /// Last-resort regex sweep over both streams.
    Generic,
}

pub(crate) fn parse_diagnostics(
    source: DiagnosticSource,
    stdout: &str,
    stderr: &str,
) -> Vec<Diagnostic> {
    match source {
        DiagnosticSource::CargoJson => {
            let mut found = parse_cargo_json(stdout);
            // Some cargo versions still print the human-rendered tail to
            // stderr for hard build failures — sweep it generically so we
            // don't drop the headline on the floor.
            if found.is_empty() {
                found.extend(parse_generic(stderr));
            }
            found
        }
        DiagnosticSource::GoBuild => {
            let mut combined = String::new();
            combined.push_str(stdout);
            combined.push('\n');
            combined.push_str(stderr);
            parse_go(&combined)
        }
        DiagnosticSource::Generic => {
            let mut combined = String::new();
            combined.push_str(stdout);
            combined.push('\n');
            combined.push_str(stderr);
            parse_generic(&combined)
        }
    }
}

fn parse_cargo_json(stdout: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if value.get("reason").and_then(|r| r.as_str()) != Some("compiler-message") {
            continue;
        }
        let message = match value.get("message") {
            Some(m) => m,
            None => continue,
        };
        let severity = message
            .get("level")
            .and_then(|l| l.as_str())
            .and_then(Severity::parse)
            .unwrap_or(Severity::Error);
        let text = message
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let primary_span = message
            .get("spans")
            .and_then(|s| s.as_array())
            .and_then(|spans| {
                spans.iter().find(|s| {
                    s.get("is_primary")
                        .and_then(|p| p.as_bool())
                        .unwrap_or(false)
                })
            });
        let (path, line, column) = match primary_span {
            Some(span) => (
                span.get("file_name")
                    .and_then(|f| f.as_str())
                    .map(str::to_string),
                span.get("line_start").and_then(|l| l.as_i64()),
                span.get("column_start").and_then(|c| c.as_i64()),
            ),
            None => (None, None, None),
        };
        out.push(Diagnostic {
            severity,
            message: text,
            path,
            line,
            column,
        });
    }
    out
}

fn parse_go(text: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        // ./pkg/foo.go:12:3: undefined: bar
        let (head, _) = match trimmed.split_once(": ") {
            Some(parts) => parts,
            None => continue,
        };
        let mut iter = head.splitn(3, ':');
        let path = iter.next();
        let line_no = iter.next();
        let col = iter.next();
        let (Some(path), Some(line_no)) = (path, line_no) else {
            continue;
        };
        let Ok(line_no) = line_no.parse::<i64>() else {
            continue;
        };
        let column = col.and_then(|c| c.parse::<i64>().ok());
        let message = trimmed[head.len() + 2..].to_string();
        if message.is_empty() {
            continue;
        }
        out.push(Diagnostic {
            severity: Severity::Error,
            message,
            path: Some(path.to_string()),
            line: Some(line_no),
            column,
        });
    }
    out
}

fn parse_generic(text: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(d) = parse_generic_line(trimmed) {
            out.push(d);
        }
    }
    out
}

fn parse_generic_line(line: &str) -> Option<Diagnostic> {
    const SEVERITIES: &[&str] = &["fatal error", "error", "warning", "warn", "note", "help"];

    // Pattern A: <head>: <severity>: <message> — head becomes path[:line[:col]].
    for sev in SEVERITIES {
        let needle = format!(": {sev}: ");
        if let Some(idx) = line.find(needle.as_str()) {
            let head = &line[..idx];
            let message = &line[idx + needle.len()..];
            let (path, line_no, column) = parse_path_position(head)?;
            return Some(Diagnostic {
                severity: Severity::parse(sev).unwrap(),
                message: message.trim().to_string(),
                path: Some(path),
                line: line_no,
                column,
            });
        }
    }
    // Pattern B: <severity>: <message> (no path).
    for sev in SEVERITIES {
        let prefix = format!("{sev}: ");
        if let Some(stripped) = line.strip_prefix(prefix.as_str()) {
            return Some(Diagnostic {
                severity: Severity::parse(sev).unwrap(),
                message: stripped.trim().to_string(),
                path: None,
                line: None,
                column: None,
            });
        }
    }
    None
}

fn parse_path_position(text: &str) -> Option<(String, Option<i64>, Option<i64>)> {
    // Accept path, path:line, or path:line:col.
    let mut parts = text.split(':').collect::<Vec<_>>();
    if parts.is_empty() {
        return None;
    }
    let mut column = None;
    let mut line = None;
    if parts.len() >= 3 {
        if let Ok(c) = parts[parts.len() - 1].parse::<i64>() {
            if let Ok(l) = parts[parts.len() - 2].parse::<i64>() {
                column = Some(c);
                line = Some(l);
                parts.truncate(parts.len() - 2);
            }
        }
    }
    if line.is_none() && parts.len() >= 2 {
        if let Ok(l) = parts[parts.len() - 1].parse::<i64>() {
            line = Some(l);
            parts.truncate(parts.len() - 1);
        }
    }
    let path = parts.join(":");
    if path.is_empty() {
        return None;
    }
    Some((path, line, column))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cargo_json_diagnostic() {
        let stdout = r#"{"reason":"compiler-message","message":{"message":"cannot find value `x` in this scope","level":"error","spans":[{"file_name":"src/lib.rs","line_start":3,"column_start":7,"is_primary":true}]}}"#;
        let diags = parse_diagnostics(DiagnosticSource::CargoJson, stdout, "");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(diags[0].path.as_deref(), Some("src/lib.rs"));
        assert_eq!(diags[0].line, Some(3));
        assert_eq!(diags[0].column, Some(7));
    }

    #[test]
    fn parses_go_path_line_col() {
        let stderr = "./pkg/foo.go:12:3: undefined: bar\n";
        let diags = parse_diagnostics(DiagnosticSource::GoBuild, "", stderr);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].path.as_deref(), Some("./pkg/foo.go"));
        assert_eq!(diags[0].line, Some(12));
        assert_eq!(diags[0].column, Some(3));
        assert_eq!(diags[0].message, "undefined: bar");
    }

    #[test]
    fn generic_picks_up_clang_style_error() {
        let stderr = "src/main.cpp:42:10: error: expected ';' before '}' token\n";
        let diags = parse_diagnostics(DiagnosticSource::Generic, "", stderr);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].path.as_deref(), Some("src/main.cpp"));
        assert_eq!(diags[0].line, Some(42));
        assert_eq!(diags[0].column, Some(10));
    }

    #[test]
    fn generic_picks_up_severity_only() {
        let stderr = "warning: deprecated API call\n";
        let diags = parse_diagnostics(DiagnosticSource::Generic, "", stderr);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Warning);
        assert_eq!(diags[0].message, "deprecated API call");
        assert!(diags[0].path.is_none());
    }
}
