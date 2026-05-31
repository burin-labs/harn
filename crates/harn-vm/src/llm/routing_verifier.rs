//! Verifier-signal escalation for `routing_policy`.
//!
//! A verifier inspects an LLM candidate's text and emits a signal in
//! `{accept, refine, escalate}`. `RoutingPolicyConfig.escalate_on`
//! holds an ordered chain of verifiers — the first non-`accept` signal
//! drives the next routing decision:
//!
//! - `accept`: keep the candidate and return it from the chain.
//! - `refine`: retry the **same** chain link with a tightened prompt;
//!   the reason text is appended as a corrective user message.
//! - `escalate`: advance to the next chain link (typically the
//!   frontier-tier model).
//!
//! Verifiers run after a successful provider call and are cheap by
//! construction — type-check / lint stay in-process via harn-parser;
//! test-run shells out under a per-verifier timeout.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use harn_parser::DiagnosticSeverity;
use regex::Regex;

use crate::value::{VmError, VmValue};

/// Maximum bytes piped into a `test_run` verifier's stdin. Caps the
/// host-side blast radius if a model emits a runaway response.
const TEST_RUN_STDIN_CAP_BYTES: usize = 256 * 1024;

const DEFAULT_TEST_RUN_TIMEOUT_SECS: u64 = 30;

/// What a verifier wants the router to do with this candidate.
///
/// Reason text is plumbed back into telemetry, receipts, and the
/// refine-nudge message so transcripts can show *why* a verifier
/// rejected an answer rather than just *that* it did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum VerifierSignal {
    Accept,
    Refine { reason: String },
    Escalate { reason: String },
}

impl VerifierSignal {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Refine { .. } => "refine",
            Self::Escalate { .. } => "escalate",
        }
    }

    pub(crate) fn reason(&self) -> Option<&str> {
        match self {
            Self::Accept => None,
            Self::Refine { reason } | Self::Escalate { reason } => Some(reason.as_str()),
        }
    }
}

/// Whether a verifier's failure mode bumps to the next link
/// (`Escalate`) or retries the same link with a nudge (`Refine`).
/// Each verifier kind has a defensible default but scripts can flip
/// it via `on_fail` in the spec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailMode {
    Escalate,
    Refine,
}

impl FailMode {
    fn parse(value: &str) -> Result<Self, VmError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "escalate" | "" => Ok(Self::Escalate),
            "refine" => Ok(Self::Refine),
            other => Err(runtime_error(format!(
                "routing_policy.escalate_on[*].on_fail: expected \"escalate\" or \"refine\", got {other:?}"
            ))),
        }
    }
}

/// A parsed verifier ready to dispatch against a candidate. The kinds
/// are deliberately closed: extending them requires a new variant so
/// the script-facing spec, telemetry name, and receipt rendering all
/// stay in lockstep.
#[derive(Clone, Debug)]
pub(crate) enum Verifier {
    TypeCheck {
        name: String,
        extract_fenced: bool,
        on_fail: FailMode,
    },
    Lint {
        name: String,
        forbidden: Vec<Regex>,
        forbidden_sources: Vec<String>,
        required: Vec<Regex>,
        required_sources: Vec<String>,
        max_line_length: Option<usize>,
        on_fail: FailMode,
    },
    TestRun {
        name: String,
        command: Vec<String>,
        timeout: Duration,
        pass_via_stdin: bool,
        on_fail: FailMode,
    },
}

impl Verifier {
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::TypeCheck { name, .. } | Self::Lint { name, .. } | Self::TestRun { name, .. } => {
                name.as_str()
            }
        }
    }

    pub(crate) fn kind_label(&self) -> &'static str {
        match self {
            Self::TypeCheck { .. } => "typecheck",
            Self::Lint { .. } => "lint",
            Self::TestRun { .. } => "test_run",
        }
    }
}

/// Run a verifier against `candidate_text`. Returns `Accept` when the
/// verifier passes; otherwise `Refine` or `Escalate` per the verifier's
/// `on_fail` mode. The async surface is uniform across kinds so the
/// caller can `await` the chain without branching on kind.
pub(crate) async fn run_verifier(verifier: &Verifier, candidate_text: &str) -> VerifierSignal {
    match verifier {
        Verifier::TypeCheck {
            extract_fenced,
            on_fail,
            ..
        } => run_typecheck(candidate_text, *extract_fenced, *on_fail),
        Verifier::Lint {
            forbidden,
            forbidden_sources,
            required,
            required_sources,
            max_line_length,
            on_fail,
            ..
        } => run_lint(
            candidate_text,
            forbidden,
            forbidden_sources,
            required,
            required_sources,
            *max_line_length,
            *on_fail,
        ),
        Verifier::TestRun {
            command,
            timeout,
            pass_via_stdin,
            on_fail,
            ..
        } => run_test_command(candidate_text, command, *timeout, *pass_via_stdin, *on_fail).await,
    }
}

fn run_typecheck(candidate_text: &str, extract_fenced: bool, on_fail: FailMode) -> VerifierSignal {
    let source = if extract_fenced {
        extract_fenced_blocks(candidate_text, &["harn"])
    } else {
        candidate_text.to_string()
    };
    if source.trim().is_empty() {
        return signal_for(
            on_fail,
            "typecheck: no Harn source found in candidate text".to_string(),
        );
    }
    match harn_parser::check_source(&source) {
        Err(err) => signal_for(on_fail, format!("typecheck: parse failed: {err}")),
        Ok((_, diagnostics)) => {
            let errors: Vec<String> = diagnostics
                .iter()
                .filter(|d| d.severity == DiagnosticSeverity::Error)
                .map(|d| d.message.clone())
                .collect();
            if errors.is_empty() {
                VerifierSignal::Accept
            } else {
                signal_for(on_fail, format!("typecheck: {}", errors.join("; ")))
            }
        }
    }
}

fn run_lint(
    candidate_text: &str,
    forbidden: &[Regex],
    forbidden_sources: &[String],
    required: &[Regex],
    required_sources: &[String],
    max_line_length: Option<usize>,
    on_fail: FailMode,
) -> VerifierSignal {
    let mut issues: Vec<String> = Vec::new();
    for (idx, re) in forbidden.iter().enumerate() {
        if re.is_match(candidate_text) {
            let pattern = forbidden_sources
                .get(idx)
                .cloned()
                .unwrap_or_else(|| re.as_str().to_string());
            issues.push(format!("forbidden pattern matched: {pattern}"));
        }
    }
    for (idx, re) in required.iter().enumerate() {
        if !re.is_match(candidate_text) {
            let pattern = required_sources
                .get(idx)
                .cloned()
                .unwrap_or_else(|| re.as_str().to_string());
            issues.push(format!("required pattern missing: {pattern}"));
        }
    }
    if let Some(limit) = max_line_length {
        if let Some((line_no, _)) = candidate_text
            .lines()
            .enumerate()
            .find(|(_, line)| line.chars().count() > limit)
        {
            issues.push(format!(
                "line {} exceeds max_line_length={limit}",
                line_no + 1
            ));
        }
    }
    if issues.is_empty() {
        VerifierSignal::Accept
    } else {
        signal_for(on_fail, format!("lint: {}", issues.join("; ")))
    }
}

async fn run_test_command(
    candidate_text: &str,
    command: &[String],
    timeout: Duration,
    pass_via_stdin: bool,
    on_fail: FailMode,
) -> VerifierSignal {
    if command.is_empty() {
        return signal_for(
            on_fail,
            "test_run: command is empty (configure routing_policy.escalate_on[*].command)"
                .to_string(),
        );
    }
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;

    let mut cmd = Command::new(&command[0]);
    cmd.args(&command[1..]);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    // `wait_with_output` consumes the Child handle, so a timeout that
    // fires while we're awaiting it would otherwise leak the OS
    // process. `kill_on_drop` makes the OS reap the child when the
    // Child handle drops, which happens precisely when the timeout
    // future is cancelled below.
    cmd.kill_on_drop(true);
    if pass_via_stdin {
        cmd.stdin(std::process::Stdio::piped());
    } else {
        cmd.stdin(std::process::Stdio::null());
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(err) => {
            return signal_for(
                on_fail,
                format!("test_run: failed to spawn {command:?}: {err}"),
            );
        }
    };

    if pass_via_stdin {
        if let Some(mut stdin) = child.stdin.take() {
            let mut bytes = candidate_text.as_bytes();
            if bytes.len() > TEST_RUN_STDIN_CAP_BYTES {
                bytes = &bytes[..TEST_RUN_STDIN_CAP_BYTES];
            }
            let _ = stdin.write_all(bytes).await;
            let _ = stdin.shutdown().await;
        }
    }

    let wait = tokio::time::timeout(timeout, child.wait_with_output()).await;
    match wait {
        Err(_) => signal_for(
            on_fail,
            format!("test_run: timed out after {}s", timeout.as_secs()),
        ),
        Ok(Err(err)) => signal_for(on_fail, format!("test_run: wait failed: {err}")),
        Ok(Ok(output)) => {
            if output.status.success() {
                VerifierSignal::Accept
            } else {
                let mut tail = String::from_utf8_lossy(&output.stderr).into_owned();
                if tail.trim().is_empty() {
                    tail = String::from_utf8_lossy(&output.stdout).into_owned();
                }
                let summary = summarize_command_output(&tail);
                signal_for(
                    on_fail,
                    format!(
                        "test_run: command exited with status {}: {summary}",
                        output
                            .status
                            .code()
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "signal".to_string())
                    ),
                )
            }
        }
    }
}

fn summarize_command_output(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "(no output)".to_string();
    }
    // Last non-empty line is usually the actionable summary
    // ("error: cannot find ..."). Cap at a sensible width so receipt
    // payloads stay readable in transcripts.
    let last = trimmed
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(trimmed);
    let mut out: String = last.chars().take(240).collect();
    if last.chars().count() > 240 {
        out.push('…');
    }
    out
}

fn signal_for(mode: FailMode, reason: String) -> VerifierSignal {
    match mode {
        FailMode::Refine => VerifierSignal::Refine { reason },
        FailMode::Escalate => VerifierSignal::Escalate { reason },
    }
}

/// Extract ```harn / ``` (or other named) fenced blocks. Falls back to
/// the entire text when no fences match — preserves typecheck coverage
/// for models that emit bare Harn source.
fn extract_fenced_blocks(text: &str, langs: &[&str]) -> String {
    let mut out = String::new();
    let mut chars = text.char_indices().peekable();
    while let Some((idx, _)) = chars.next() {
        if !text[idx..].starts_with("```") {
            continue;
        }
        let after_fence = &text[idx + 3..];
        let header_end = after_fence.find('\n').unwrap_or(after_fence.len());
        let lang = after_fence[..header_end].trim();
        let body_start = idx + 3 + header_end + 1;
        let Some(rel_end) = text[body_start..].find("```") else {
            break;
        };
        let body_end = body_start + rel_end;
        if lang.is_empty() || langs.iter().any(|l| lang.eq_ignore_ascii_case(l)) {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text[body_start..body_end].trim_end_matches('\n'));
        }
        // Skip ahead so the outer iterator doesn't try to re-parse the
        // characters we just consumed.
        while let Some((next_idx, _)) = chars.peek() {
            if *next_idx >= body_end + 3 {
                break;
            }
            chars.next();
        }
    }
    if out.is_empty() {
        text.to_string()
    } else {
        out
    }
}

// ---------------------------------------------------------------------------
// Refine nudges
// ---------------------------------------------------------------------------

/// Compose a corrective user message from one or more refine reasons.
/// Used by the routing executor when retrying the same link.
pub(crate) fn build_refine_nudge(reasons: &[String]) -> String {
    if reasons.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "The previous response did not pass verification. Please address the following and try again:\n",
    );
    for reason in reasons {
        out.push_str("- ");
        out.push_str(reason);
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// Config parsing — turn a Harn-side spec into `Vec<Verifier>`
// ---------------------------------------------------------------------------

fn runtime_error(message: String) -> VmError {
    VmError::Thrown(VmValue::String(std::sync::Arc::from(message)))
}

fn parse_string(dict: &BTreeMap<String, VmValue>, key: &str) -> Result<Option<String>, VmError> {
    match dict.get(key) {
        Some(VmValue::Nil) | None => Ok(None),
        Some(VmValue::String(s)) => Ok(Some(s.to_string())),
        Some(other) => Err(runtime_error(format!(
            "routing_policy.escalate_on[*].{key}: expected a string, got {}",
            other.type_name()
        ))),
    }
}

fn parse_bool(dict: &BTreeMap<String, VmValue>, key: &str) -> Result<Option<bool>, VmError> {
    match dict.get(key) {
        Some(VmValue::Nil) | None => Ok(None),
        Some(VmValue::Bool(b)) => Ok(Some(*b)),
        Some(other) => Err(runtime_error(format!(
            "routing_policy.escalate_on[*].{key}: expected a boolean, got {}",
            other.type_name()
        ))),
    }
}

fn parse_pos_usize(dict: &BTreeMap<String, VmValue>, key: &str) -> Result<Option<usize>, VmError> {
    match dict.get(key) {
        Some(VmValue::Nil) | None => Ok(None),
        Some(VmValue::Int(n)) if *n >= 0 => Ok(Some(*n as usize)),
        Some(other) => Err(runtime_error(format!(
            "routing_policy.escalate_on[*].{key}: expected a non-negative integer, got {}",
            other.type_name()
        ))),
    }
}

fn parse_regex_list(
    dict: &BTreeMap<String, VmValue>,
    key: &str,
) -> Result<(Vec<Regex>, Vec<String>), VmError> {
    let value = match dict.get(key) {
        None | Some(VmValue::Nil) => return Ok((Vec::new(), Vec::new())),
        Some(v) => v,
    };
    let items = match value {
        VmValue::List(items) => items.clone(),
        VmValue::String(s) => Arc::new(vec![VmValue::String(s.clone())]),
        other => {
            return Err(runtime_error(format!(
                "routing_policy.escalate_on[*].{key}: expected a list of regex strings, got {}",
                other.type_name()
            )));
        }
    };
    let mut compiled = Vec::with_capacity(items.len());
    let mut sources = Vec::with_capacity(items.len());
    for item in items.iter() {
        let pattern = match item {
            VmValue::String(s) => s.to_string(),
            other => {
                return Err(runtime_error(format!(
                    "routing_policy.escalate_on[*].{key}: list entries must be regex strings, got {}",
                    other.type_name()
                )));
            }
        };
        let trimmed = pattern.trim();
        if trimmed.is_empty() {
            continue;
        }
        let re = Regex::new(trimmed).map_err(|err| {
            runtime_error(format!(
                "routing_policy.escalate_on[*].{key}: invalid regex {trimmed:?}: {err}"
            ))
        })?;
        compiled.push(re);
        sources.push(trimmed.to_string());
    }
    Ok((compiled, sources))
}

fn parse_command(value: Option<&VmValue>) -> Result<Vec<String>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(Vec::new()),
        Some(VmValue::String(s)) => {
            // Split on whitespace for the common simple-command case;
            // structured args go through the list form.
            Ok(s.split_whitespace().map(str::to_string).collect())
        }
        Some(VmValue::List(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for (idx, item) in items.iter().enumerate() {
                match item {
                    VmValue::String(s) => out.push(s.to_string()),
                    other => {
                        return Err(runtime_error(format!(
                            "routing_policy.escalate_on[*].command[{idx}]: expected string, got {}",
                            other.type_name()
                        )));
                    }
                }
            }
            Ok(out)
        }
        Some(other) => Err(runtime_error(format!(
            "routing_policy.escalate_on[*].command: expected string or list, got {}",
            other.type_name()
        ))),
    }
}

fn parse_on_fail(dict: &BTreeMap<String, VmValue>, default: FailMode) -> Result<FailMode, VmError> {
    match dict.get("on_fail") {
        Some(VmValue::Nil) | None => Ok(default),
        Some(VmValue::String(s)) => FailMode::parse(s),
        Some(other) => Err(runtime_error(format!(
            "routing_policy.escalate_on[*].on_fail: expected a string, got {}",
            other.type_name()
        ))),
    }
}

fn parse_one_verifier(value: &VmValue, idx: usize) -> Result<Verifier, VmError> {
    let dict = match value {
        VmValue::Dict(dict) => dict.clone(),
        VmValue::String(target) => {
            // Shorthand: "typecheck" / "lint" → all defaults.
            return shorthand_verifier(target, idx);
        }
        other => {
            return Err(runtime_error(format!(
                "routing_policy.escalate_on[{idx}]: expected dict or string, got {}",
                other.type_name()
            )));
        }
    };
    let kind = dict
        .get("kind")
        .map(|v| v.display())
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let name = parse_string(&dict, "name")?.unwrap_or_else(|| match kind.as_str() {
        "typecheck" | "type_check" | "type-check" => "typecheck".to_string(),
        "lint" => "lint".to_string(),
        "test_run" | "testrun" | "test-run" | "test" => "test_run".to_string(),
        other => other.to_string(),
    });
    match kind.as_str() {
        "typecheck" | "type_check" | "type-check" => {
            let extract_fenced = parse_bool(&dict, "extract_fenced")?.unwrap_or(true);
            let on_fail = parse_on_fail(&dict, FailMode::Escalate)?;
            Ok(Verifier::TypeCheck {
                name,
                extract_fenced,
                on_fail,
            })
        }
        "lint" => {
            let (forbidden, forbidden_sources) = parse_regex_list(&dict, "forbidden_patterns")?;
            let (required, required_sources) = parse_regex_list(&dict, "required_patterns")?;
            let max_line_length = parse_pos_usize(&dict, "max_line_length")?;
            let on_fail = parse_on_fail(&dict, FailMode::Refine)?;
            if forbidden.is_empty() && required.is_empty() && max_line_length.is_none() {
                return Err(runtime_error(format!(
                    "routing_policy.escalate_on[{idx}]: lint verifier needs at least one of forbidden_patterns, required_patterns, or max_line_length"
                )));
            }
            Ok(Verifier::Lint {
                name,
                forbidden,
                forbidden_sources,
                required,
                required_sources,
                max_line_length,
                on_fail,
            })
        }
        "test_run" | "testrun" | "test-run" | "test" => {
            let command = parse_command(dict.get("command"))?;
            if command.is_empty() {
                return Err(runtime_error(format!(
                    "routing_policy.escalate_on[{idx}]: test_run verifier requires a non-empty `command`"
                )));
            }
            let timeout_secs = match dict.get("timeout_secs") {
                Some(VmValue::Nil) | None => DEFAULT_TEST_RUN_TIMEOUT_SECS,
                Some(VmValue::Int(n)) if *n > 0 => *n as u64,
                Some(other) => {
                    return Err(runtime_error(format!(
                        "routing_policy.escalate_on[{idx}].timeout_secs: expected a positive integer, got {}",
                        other.type_name()
                    )));
                }
            };
            let pass_via_stdin = parse_bool(&dict, "pass_via_stdin")?.unwrap_or(true);
            let on_fail = parse_on_fail(&dict, FailMode::Escalate)?;
            Ok(Verifier::TestRun {
                name,
                command,
                timeout: Duration::from_secs(timeout_secs),
                pass_via_stdin,
                on_fail,
            })
        }
        "" => Err(runtime_error(format!(
            "routing_policy.escalate_on[{idx}]: `kind` is required (one of typecheck, lint, test_run)"
        ))),
        other => Err(runtime_error(format!(
            "routing_policy.escalate_on[{idx}]: unknown kind {other:?} (expected typecheck, lint, or test_run)"
        ))),
    }
}

fn shorthand_verifier(kind: &str, idx: usize) -> Result<Verifier, VmError> {
    let normalized = kind.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "typecheck" | "type_check" | "type-check" => Ok(Verifier::TypeCheck {
            name: "typecheck".to_string(),
            extract_fenced: true,
            on_fail: FailMode::Escalate,
        }),
        // Lint and test_run can't take a sensible default: lint needs
        // at least one pattern or a length cap, test_run needs a
        // command. Bail loudly so the user fixes their config.
        "lint" => Err(runtime_error(format!(
            "routing_policy.escalate_on[{idx}]: shorthand \"lint\" needs at least one of forbidden_patterns, required_patterns, or max_line_length — use a dict spec"
        ))),
        "test_run" | "testrun" | "test-run" | "test" => Err(runtime_error(format!(
            "routing_policy.escalate_on[{idx}]: shorthand \"test_run\" needs a command — use a dict spec"
        ))),
        other => Err(runtime_error(format!(
            "routing_policy.escalate_on[{idx}]: unknown shorthand verifier {other:?}"
        ))),
    }
}

/// Public entry: parse the `escalate_on` field. Empty list / nil →
/// `Ok(Vec::new())` so the legacy fallback path runs.
pub(crate) fn parse_escalate_on(value: Option<&VmValue>) -> Result<Vec<Verifier>, VmError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let items = match value {
        VmValue::Nil => return Ok(Vec::new()),
        VmValue::List(items) => items.clone(),
        VmValue::Dict(_) | VmValue::String(_) => Arc::new(vec![value.clone()]),
        other => {
            return Err(runtime_error(format!(
                "routing_policy.escalate_on: expected a list of verifier specs, got {}",
                other.type_name()
            )));
        }
    };
    let mut out = Vec::with_capacity(items.len());
    for (idx, item) in items.iter().enumerate() {
        out.push(parse_one_verifier(item, idx)?);
    }
    Ok(out)
}

/// Render verifier specs into a summary `VmValue` for the tagged
/// `routing_policy(...)` dict — keeps script-side debugging readable
/// (`policy.escalate_on` mirrors what the parser ingested).
pub(crate) fn verifiers_summary(verifiers: &[Verifier]) -> VmValue {
    let items: Vec<VmValue> = verifiers
        .iter()
        .map(|v| {
            let mut dict = BTreeMap::new();
            dict.insert(
                "kind".to_string(),
                VmValue::String(std::sync::Arc::from(v.kind_label())),
            );
            dict.insert(
                "name".to_string(),
                VmValue::String(std::sync::Arc::from(v.name().to_string())),
            );
            let on_fail = match v {
                Verifier::TypeCheck { on_fail, .. }
                | Verifier::Lint { on_fail, .. }
                | Verifier::TestRun { on_fail, .. } => *on_fail,
            };
            dict.insert(
                "on_fail".to_string(),
                VmValue::String(std::sync::Arc::from(match on_fail {
                    FailMode::Refine => "refine",
                    FailMode::Escalate => "escalate",
                })),
            );
            VmValue::Dict(std::sync::Arc::new(dict))
        })
        .collect();
    VmValue::List(std::sync::Arc::new(items))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(items: &[(&str, VmValue)]) -> BTreeMap<String, VmValue> {
        items
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn shorthand_typecheck_parses() {
        let spec = VmValue::List(std::sync::Arc::new(vec![VmValue::String(
            std::sync::Arc::from("typecheck"),
        )]));
        let verifiers = parse_escalate_on(Some(&spec)).expect("parses");
        assert_eq!(verifiers.len(), 1);
        assert!(matches!(verifiers[0], Verifier::TypeCheck { .. }));
    }

    #[test]
    fn lint_requires_at_least_one_rule() {
        let spec = VmValue::List(std::sync::Arc::new(vec![VmValue::Dict(
            std::sync::Arc::new(dict(&[(
                "kind",
                VmValue::String(std::sync::Arc::from("lint")),
            )])),
        )]));
        let err = parse_escalate_on(Some(&spec)).unwrap_err();
        let message = match err {
            VmError::Thrown(VmValue::String(s)) => s.to_string(),
            other => panic!("unexpected error: {other:?}"),
        };
        assert!(message.contains("at least one"));
    }

    #[test]
    fn test_run_requires_command() {
        let spec = VmValue::List(std::sync::Arc::new(vec![VmValue::Dict(
            std::sync::Arc::new(dict(&[(
                "kind",
                VmValue::String(std::sync::Arc::from("test_run")),
            )])),
        )]));
        let err = parse_escalate_on(Some(&spec)).unwrap_err();
        let message = match err {
            VmError::Thrown(VmValue::String(s)) => s.to_string(),
            other => panic!("unexpected error: {other:?}"),
        };
        assert!(message.contains("command"));
    }

    #[test]
    fn lint_verifier_signals_refine_on_forbidden_pattern() {
        let verifier = Verifier::Lint {
            name: "lint".to_string(),
            forbidden: vec![Regex::new(r"(?i)unwrap\s*\(\s*\)").unwrap()],
            forbidden_sources: vec!["unwrap()".to_string()],
            required: Vec::new(),
            required_sources: Vec::new(),
            max_line_length: None,
            on_fail: FailMode::Refine,
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let signal = rt.block_on(run_verifier(&verifier, "let x = foo.unwrap();"));
        match signal {
            VerifierSignal::Refine { reason } => assert!(reason.contains("unwrap")),
            other => panic!("expected refine, got {other:?}"),
        }
    }

    #[test]
    fn lint_verifier_accepts_clean_text() {
        let verifier = Verifier::Lint {
            name: "lint".to_string(),
            forbidden: vec![Regex::new(r"\bTODO\b|\bFIXME\b").unwrap()],
            forbidden_sources: vec!["TODO|FIXME".to_string()],
            required: Vec::new(),
            required_sources: Vec::new(),
            max_line_length: Some(120),
            on_fail: FailMode::Escalate,
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let signal = rt.block_on(run_verifier(&verifier, "let x = 1\n"));
        assert_eq!(signal, VerifierSignal::Accept);
    }

    #[test]
    fn typecheck_verifier_accepts_valid_harn() {
        let verifier = Verifier::TypeCheck {
            name: "typecheck".to_string(),
            extract_fenced: false,
            on_fail: FailMode::Escalate,
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let signal = rt.block_on(run_verifier(&verifier, "let x = 1\n"));
        assert_eq!(signal, VerifierSignal::Accept);
    }

    #[test]
    fn typecheck_verifier_escalates_on_parse_failure() {
        let verifier = Verifier::TypeCheck {
            name: "typecheck".to_string(),
            extract_fenced: false,
            on_fail: FailMode::Escalate,
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let signal = rt.block_on(run_verifier(&verifier, "let x = @@@\n"));
        assert!(matches!(signal, VerifierSignal::Escalate { .. }));
    }

    #[test]
    fn typecheck_extracts_fenced_harn_block() {
        let verifier = Verifier::TypeCheck {
            name: "typecheck".to_string(),
            extract_fenced: true,
            on_fail: FailMode::Escalate,
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let candidate = "Here's the fix:\n```harn\nlet x = 1\n```\nDone.";
        let signal = rt.block_on(run_verifier(&verifier, candidate));
        assert_eq!(signal, VerifierSignal::Accept);
    }

    #[test]
    fn build_refine_nudge_groups_reasons() {
        let nudge = build_refine_nudge(&[
            "lint: forbidden pattern matched".to_string(),
            "typecheck: parse failed".to_string(),
        ]);
        assert!(nudge.contains("lint:"));
        assert!(nudge.contains("typecheck:"));
        assert!(nudge.contains("did not pass verification"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_run_verifier_accepts_zero_exit() {
        let verifier = Verifier::TestRun {
            name: "test_run".to_string(),
            command: vec!["true".to_string()],
            timeout: Duration::from_secs(5),
            pass_via_stdin: false,
            on_fail: FailMode::Escalate,
        };
        let signal = run_verifier(&verifier, "anything").await;
        assert_eq!(signal, VerifierSignal::Accept);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_run_verifier_escalates_on_nonzero_exit() {
        let verifier = Verifier::TestRun {
            name: "test_run".to_string(),
            command: vec!["false".to_string()],
            timeout: Duration::from_secs(5),
            pass_via_stdin: false,
            on_fail: FailMode::Escalate,
        };
        let signal = run_verifier(&verifier, "anything").await;
        assert!(matches!(signal, VerifierSignal::Escalate { .. }));
    }
}
