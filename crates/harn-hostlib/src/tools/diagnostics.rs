//! Pure compiler-output decoding shared by captured-output consumers and builds.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::HostlibError;

pub(crate) const NAME: &str = "hostlib_tools_decode_diagnostics";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DecodeRequest {
    pub(crate) format: String,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) exit_code: Option<i64>,
    pub(crate) completed: bool,
    pub(crate) capture_complete: bool,
    #[serde(default)]
    pub(crate) source_text: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct DecodeResult {
    pub(crate) measured: bool,
    reason: &'static str,
    error_count: Option<usize>,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

pub(crate) fn handle(args: &[harn_vm::VmValue]) -> Result<harn_vm::VmValue, HostlibError> {
    let map = super::payload::require_dict_arg(NAME, args)?;
    let request = serde_json::from_value(crate::json::vm_dict_to_json(&map)).map_err(|error| {
        HostlibError::InvalidParameter {
            builtin: NAME,
            param: "request",
            message: error.to_string(),
        }
    })?;
    Ok(harn_vm::json_to_vm_value(
        &serde_json::to_value(decode(request)).expect("diagnostic result is serializable"),
    ))
}

/// The format registry lives here, alongside its decoders. Unknown future
/// formats are an observation failure rather than a successful empty check.
pub(crate) fn decode(request: DecodeRequest) -> DecodeResult {
    let text = format!("{}\n{}", request.stdout, request.stderr);
    let format = request.format.trim().to_ascii_lowercase();
    let parsed = match format.as_str() {
        "cargo-json" => parse_cargo_checked(&request.stdout, &request.stderr),
        "ruff" | "shellcheck" => parse_json_array(&request.stdout, &format),
        "harn-json" => parse_harn_json(&request.stdout, request.source_text.as_deref()),
        "go" => Ok(parse_go(&text)),
        "generic" | "positional" => Ok(parse_generic(&text)),
        "zsh" => Ok(parse_line_format(&text, false)),
        "scala3" => Ok(parse_scala3(&text)),
        "php-lint" => Ok(parse_line_format(&text, true)),
        _ => {
            return DecodeResult {
                measured: false,
                reason: "unsupported_format",
                error_count: None,
                diagnostics: vec![],
            }
        }
    };
    let (diagnostics, malformed) = match parsed {
        Ok(rows) => (rows, false),
        Err(rows) => (rows, true),
    };
    let errors = diagnostics
        .iter()
        .filter(|row| row.severity == Severity::Error)
        .count();
    // ShellCheck exit 1 includes informational/style findings, not just errors.
    let explained_failure = errors > 0
        || (format == "shellcheck" && request.exit_code == Some(1) && !diagnostics.is_empty());
    let reason = if !request.completed || request.exit_code.is_none() {
        "process_incomplete"
    } else if !request.capture_complete {
        "incomplete_capture"
    } else if malformed {
        "malformed_output"
    } else if request.exit_code != Some(0) && !explained_failure {
        "unrecognized_failure"
    } else {
        ""
    };
    DecodeResult {
        measured: reason.is_empty(),
        reason,
        error_count: reason.is_empty().then_some(errors),
        diagnostics,
    }
}

/// Severity of a build diagnostic. Maps 1:1 to the schema enum at
/// `schemas/tools/run_build_command.response.json#/$defs/Diagnostic/properties/severity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
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
            "note" | "info" | "style" => Some(Severity::Note),
            "help" | "hint" => Some(Severity::Help),
            _ => None,
        }
    }
}

/// One parsed diagnostic. Matches the `Diagnostic` schema in
/// `run_build_command.response.json`.
#[derive(Debug, Clone, Serialize)]
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

impl DiagnosticSource {
    pub(crate) fn format(self) -> &'static str {
        match self {
            Self::CargoJson => "cargo-json",
            Self::GoBuild => "go",
            Self::Generic => "generic",
        }
    }
}

fn parse_cargo_checked(stdout: &str, stderr: &str) -> Result<Vec<Diagnostic>, Vec<Diagnostic>> {
    let malformed = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .any(|line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .is_none_or(|value| {
                    value.get("reason").and_then(Value::as_str).is_none()
                        || (value["reason"] == "compiler-message"
                            && (value["message"]["message"].as_str().is_none()
                                || value["message"]["level"]
                                    .as_str()
                                    .and_then(Severity::parse)
                                    .is_none()))
                })
        });
    let mut rows = parse_cargo_json(stdout);
    if rows.is_empty() {
        rows.extend(parse_generic(stderr));
    }
    if malformed {
        Err(rows)
    } else {
        Ok(rows)
    }
}

fn parse_json_array(raw: &str, format: &str) -> Result<Vec<Diagnostic>, Vec<Diagnostic>> {
    let Ok(Value::Array(entries)) = serde_json::from_str(raw) else {
        return Err(vec![]);
    };
    let mut rows = vec![];
    let mut malformed = false;
    for entry in entries {
        let row = (|| {
            let message = entry.get("message")?.as_str()?;
            if message.trim().is_empty() {
                return None;
            }
            let (severity, code, line, column) = if format.eq_ignore_ascii_case("ruff") {
                let code = entry.get("code")?.as_str()?;
                (
                    Severity::Error,
                    code.to_owned(),
                    entry["location"]["row"].as_i64()?,
                    entry["location"]["column"].as_i64()?,
                )
            } else {
                (
                    Severity::parse(entry.get("level")?.as_str()?)?,
                    format!("SC{}", entry.get("code")?.as_i64()?),
                    entry.get("line")?.as_i64()?,
                    entry.get("column")?.as_i64()?,
                )
            };
            if line < 1 || column < 1 {
                return None;
            }
            Some(Diagnostic {
                severity,
                message: format!("{code}: {message}"),
                path: entry
                    .get("filename")
                    .or_else(|| entry.get("file"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                line: Some(line),
                column: Some(column),
            })
        })();
        if let Some(row) = row {
            rows.push(row);
        } else {
            malformed = true;
        }
    }
    if malformed {
        Err(rows)
    } else {
        Ok(rows)
    }
}

fn parse_harn_json(raw: &str, source: Option<&str>) -> Result<Vec<Diagnostic>, Vec<Diagnostic>> {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return Err(vec![]);
    };
    let Some(files) = value["data"]["files"].as_array() else {
        return Err(vec![]);
    };
    let mut rows = vec![];
    let mut malformed = files.is_empty()
        || value["schemaVersion"].as_u64() != Some(1)
        || value["ok"].as_bool().is_none();
    let mut observed_count = 0;
    for file in files {
        malformed |= file["path"].as_str().is_none()
            || !matches!(file["status"].as_str(), Some("ok" | "warning" | "error"));
        let Some(entries) = file["diagnostics"].as_array() else {
            malformed = true;
            continue;
        };
        observed_count += entries.len();
        for entry in entries {
            let row = (|| {
                let severity = Severity::parse(entry["severity"].as_str()?)?;
                let message = entry["message"].as_str()?;
                if message.trim().is_empty() {
                    return None;
                }
                let offset = entry["span"]["start"].as_u64()? as usize;
                let line = source.map(|text| {
                    let prefix = &text.as_bytes()[..offset.min(text.len())];
                    (crate::text::count_lines(prefix)
                        + u64::from(prefix.is_empty() || prefix.ends_with(b"\n")))
                        as i64
                });
                let code = entry["code"].as_str().unwrap_or("");
                Some(Diagnostic {
                    severity,
                    message: if code.is_empty() {
                        message.to_owned()
                    } else {
                        format!("{code}: {message}")
                    },
                    path: file["path"].as_str().map(str::to_owned),
                    line,
                    column: None,
                })
            })();
            if let Some(row) = row {
                rows.push(row);
            } else {
                malformed = true;
            }
        }
    }
    malformed |= value["data"]["summary"]["diagnostics"].as_u64() != Some(observed_count as u64)
        || (value["data"]["summary"]["errors"].as_u64().unwrap_or(1) > 0
            && !rows.iter().any(|row| row.severity == Severity::Error));
    if malformed {
        Err(rows)
    } else {
        Ok(rows)
    }
}

fn parse_scala3(text: &str) -> Vec<Diagnostic> {
    static BANNER: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^--\s*(?:\[[^\]]+\]\s*)?(.+?):\s*(.+):(\d+):(\d+)\b").unwrap()
    });
    let lines: Vec<_> = text.lines().collect();
    let mut rows = vec![];
    for (index, line) in lines.iter().enumerate() {
        let Some(caps) = BANNER.captures(line) else {
            continue;
        };
        let title = caps[1].trim();
        let detail = lines[index + 1..]
            .iter()
            .map_while(|line| {
                if line.trim_start().starts_with("--") {
                    None
                } else {
                    line.split_once('|')
                }
            })
            .find_map(|(head, tail)| {
                let tail = tail.trim();
                (!tail.is_empty()
                    && head.trim().is_empty()
                    && !tail
                        .chars()
                        .all(|c| c.is_whitespace() || c == '^' || c == '~')
                    && !tail.starts_with("longer explanation available"))
                .then_some(tail)
            });
        rows.push(Diagnostic {
            severity: Severity::Error,
            message: detail.map_or_else(|| title.to_owned(), |detail| format!("{title}: {detail}")),
            path: Some(caps[2].to_owned()),
            line: caps[3].parse().ok(),
            column: caps[4].parse().ok(),
        });
    }
    rows
}

fn parse_line_format(text: &str, php: bool) -> Vec<Diagnostic> {
    static PHP: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^(?:PHP\s+)?Parse error:\s*(.+)\s+in\s+(.+)\s+on\s+line\s+(\d+)\s*$")
            .unwrap()
    });
    static ZSH: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"^(.+):(\d+):\s*(.+)$").unwrap());
    text.lines()
        .filter_map(|line| {
            let caps = if php {
                PHP.captures(line)
            } else {
                ZSH.captures(line)
            }?;
            let (path, row, message) = if php {
                (&caps[2], &caps[3], &caps[1])
            } else {
                (&caps[1], &caps[2], &caps[3])
            };
            Some(Diagnostic {
                severity: Severity::Error,
                message: message.trim().to_owned(),
                path: Some(path.to_owned()),
                line: row.parse().ok(),
                column: None,
            })
        })
        .collect()
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
        let (head, tail) = match trimmed.split_once(": ") {
            Some(parts) => parts,
            None => continue,
        };
        let Some((path, Some(line_no), column)) = parse_path_position(head) else {
            continue;
        };
        let message = tail.to_string();
        if message.is_empty() {
            continue;
        }
        out.push(Diagnostic {
            severity: Severity::Error,
            message,
            path: Some(path),
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

    // rustc's short format includes an error code between severity and colon.
    if let Some((head, coded)) = line.split_once(": error[") {
        if let Some((_, message)) = coded.split_once("]: ") {
            let (path, line, column) = parse_path_position(head)?;
            return Some(Diagnostic {
                severity: Severity::Error,
                message: message.trim().to_owned(),
                path: Some(path),
                line,
                column,
            });
        }
    }

    // Pattern A: <head>: <severity>: <message> — head becomes path[:line[:col]].
    for sev in SEVERITIES {
        let needle = format!(": {sev}: ");
        if let Some((head, message)) = line.split_once(needle.as_str()) {
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
        let diags = parse_cargo_checked(stdout, "").unwrap();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(diags[0].path.as_deref(), Some("src/lib.rs"));
        assert_eq!(diags[0].line, Some(3));
        assert_eq!(diags[0].column, Some(7));
    }

    #[test]
    fn parses_go_path_line_col() {
        let stderr = "./pkg/foo.go:12:3: undefined: bar\n";
        let diags = parse_go(stderr);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].path.as_deref(), Some("./pkg/foo.go"));
        assert_eq!(diags[0].line, Some(12));
        assert_eq!(diags[0].column, Some(3));
        assert_eq!(diags[0].message, "undefined: bar");
    }

    #[test]
    fn generic_picks_up_clang_style_error() {
        let stderr = "src/main.cpp:42:10: error: expected ';' before '}' token\n";
        let diags = parse_generic(stderr);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].path.as_deref(), Some("src/main.cpp"));
        assert_eq!(diags[0].line, Some(42));
        assert_eq!(diags[0].column, Some(10));
    }

    #[test]
    fn generic_picks_up_severity_only() {
        let stderr = "warning: deprecated API call\n";
        let diags = parse_generic(stderr);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Warning);
        assert_eq!(diags[0].message, "deprecated API call");
        assert!(diags[0].path.is_none());
    }
}
