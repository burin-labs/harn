//! Free-form string secret patterns reused for redaction.
//!
//! Each pattern is named so the replacement placeholder is
//! `<redacted:<pattern_name>:<len>>` and audit events can attribute the
//! redaction to a specific provider. The [`crate::stdlib::secret_scan`]
//! module defines the canonical set of high-confidence secret regexes
//! that the `secret_scan` builtin reports to scripts; this module
//! borrows from that same set so a string that scanning would flag is
//! also a string that redaction will scrub — there is one definition,
//! not two.
//!
//! # Custom patterns
//!
//! Hosts and scripts can register additional named patterns through
//! [`register_custom_pattern`]. Custom patterns live on a thread-local
//! stack so test pollution stays contained and so a per-orchestrator
//! override can be installed alongside the existing
//! [`crate::redact::PolicyGuard`].
//!
//! # Audit
//!
//! Every redaction synchronously records a [`RedactionEvent`] in a
//! per-thread ring drainable via [`drain_audit_ring`], and also fires
//! an optional [`AuditSink`] callback. The default sink installed by
//! the [`crate::stdlib::token_redaction`] stdlib forwards events to
//! the live events pipeline and, on a multi-threaded Tokio runtime,
//! to the `audit.token_redaction` event-log topic. Audit entries
//! carry the diagnostic identifier `HARN-OAU-001` from the OA-06
//! epic — they never include the raw token.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;

/// Stable identifier emitted in audit logs for every token-redaction
/// event. Part of the OA-06 epic's compliance contract.
pub const TOKEN_REDACTION_DIAGNOSTIC: &str = "HARN-OAU-001";

/// Event-log topic used for token-redaction audit events.
pub const TOKEN_REDACTION_AUDIT_TOPIC: &str = "audit.token_redaction";

/// Upper bound on input length scanned by the secret detector. Inputs
/// above this size short-circuit to "no redaction" so a pathological
/// caller cannot trigger catastrophic regex behavior on the persistence
/// hot path. Persisted JSON payloads larger than this are already
/// abnormal; the receipt/event-log layers already cap message sizes
/// well below this in practice.
const MAX_SCAN_INPUT_BYTES: usize = 256 * 1024;

/// One redaction pattern with a stable display name.
#[derive(Clone)]
pub struct NamedPattern {
    /// Short, kebab-case identifier (e.g. `"github_pat_classic"`).
    /// Stable across versions — emitted in audit events and in the
    /// `<redacted:name:len>` placeholder.
    pub name: &'static str,
    /// Compiled regex. Always anchored on `\b` or non-word boundaries
    /// so it does not chew unrelated identifiers.
    pub regex: Regex,
}

impl std::fmt::Debug for NamedPattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NamedPattern")
            .field("name", &self.name)
            .field("regex", &self.regex.as_str())
            .finish()
    }
}

/// Default token patterns shipped with Harn. Order matters only for
/// audit attribution when multiple patterns would match the same
/// substring — earlier patterns win.
pub static DEFAULT_PATTERNS: LazyLock<Vec<NamedPattern>> = LazyLock::new(|| {
    vec![
        // JWT — three base64url segments separated by `.`. Required
        // prefix `eyJ` keeps the regex from chewing arbitrary
        // dotted base64.
        NamedPattern {
            name: "jwt",
            regex: Regex::new(r"\beyJ[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]{4,}\b")
                .expect("jwt regex compiles"),
        },
        // GitHub OAuth / app installation tokens (ghp_, gho_, ghu_,
        // ghs_, ghr_).
        NamedPattern {
            name: "github_token",
            regex: Regex::new(r"\bgh[pousr]_[A-Za-z0-9]{36,255}\b")
                .expect("github_token regex compiles"),
        },
        // GitHub fine-grained personal access token.
        NamedPattern {
            name: "github_pat_fine",
            regex: Regex::new(r"\bgithub_pat_[A-Za-z0-9_]{20,255}\b")
                .expect("github_pat_fine regex compiles"),
        },
        // Slack tokens (xoxb / xoxp / xoxa / xoxs / xoxr).
        NamedPattern {
            name: "slack_token",
            regex: Regex::new(r"\bxox[abprs]-[A-Za-z0-9-]{10,255}\b")
                .expect("slack_token regex compiles"),
        },
        // AWS access key ids (gitleaks family).
        NamedPattern {
            name: "aws_access_key",
            regex: Regex::new(r"\b(?:AKIA|ASIA|AGPA|AIDA|ANPA|AROA|AIPA)[A-Z0-9]{16}\b")
                .expect("aws_access_key regex compiles"),
        },
        // GitLab personal access token.
        NamedPattern {
            name: "gitlab_token",
            regex: Regex::new(r"\bglpat-[A-Za-z0-9_-]{20,255}\b")
                .expect("gitlab_token regex compiles"),
        },
        // npm access token.
        NamedPattern {
            name: "npm_token",
            regex: Regex::new(r"\bnpm_[A-Za-z0-9]{36}\b").expect("npm_token regex compiles"),
        },
        // OpenAI / Anthropic-style sk- keys (long, project, etc).
        NamedPattern {
            name: "openai_key",
            regex: Regex::new(r"\bsk-[A-Za-z0-9_-]{20,255}\b").expect("openai_key regex compiles"),
        },
        // Stripe live/test keys.
        NamedPattern {
            name: "stripe_key",
            regex: Regex::new(r"\b(?:rk|sk)_(?:live|test)_[0-9A-Za-z]{16,255}\b")
                .expect("stripe_key regex compiles"),
        },
        // `Authorization: Bearer <token>` header form as well as
        // bare `Bearer xyz` substrings in free text.
        NamedPattern {
            name: "bearer_token",
            regex: Regex::new(r"(?i)\bBearer\s+[A-Za-z0-9._\-+/=]{12,}")
                .expect("bearer_token regex compiles"),
        },
    ]
});

thread_local! {
    /// Custom token patterns installed by stdlib callers. Stored on a
    /// per-thread stack the same way [`crate::redact::PolicyGuard`]
    /// stores active policies; `reset_thread_local_state` clears them.
    static CUSTOM_PATTERNS: RefCell<Vec<NamedPattern>> = const { RefCell::new(Vec::new()) };

    /// Callback that receives one entry per pattern that matched.
    /// Set by callers that want to audit redactions
    /// (`stdlib::token_redaction` installs a default sink that
    /// forwards to the event log when a runtime is available).
    /// `None` means "no extra audit collection on this thread".
    /// Every redaction also lands in [`AUDIT_RING`] regardless of
    /// whether a sink is installed.
    static AUDIT_SINK: RefCell<Option<AuditSink>> = const { RefCell::new(None) };

    /// Authoritative per-thread audit ring. Always populated on
    /// every redaction so the synchronous compliance contract holds
    /// in every execution context (sync host calls, single-threaded
    /// LocalSet, multi-thread runtime). Drained by stdlib via
    /// [`drain_audit_ring`].
    static AUDIT_RING: RefCell<Vec<RedactionEvent>> = const { RefCell::new(Vec::new()) };
}

/// Per-redaction event passed to an installed [`AuditSink`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedactionEvent {
    pub pattern_name: String,
    pub match_count: usize,
    /// Total bytes redacted across all matches of this pattern.
    pub bytes_redacted: usize,
}

/// Thread-local callback invoked once per pattern that matched during a
/// single `scan_secret_patterns` call.
pub type AuditSink = std::rc::Rc<dyn Fn(&RedactionEvent)>;

/// Register a custom named pattern on the calling thread. Returns an
/// error if the regex fails to compile. The pattern is appended after
/// the default catalog, so default patterns still win when multiple
/// would match the same substring.
pub fn register_custom_pattern(name: impl Into<String>, regex_source: &str) -> Result<(), String> {
    let regex = Regex::new(regex_source).map_err(|error| format!("invalid regex: {error}"))?;
    // Leak the name to `'static` so the pattern's name field stays
    // borrow-free and serialization can carry the same lifetime as
    // the default catalog. Custom patterns are rare and never freed
    // — the leak is bounded by the number of distinct user-supplied
    // names per process.
    let name_static: &'static str = Box::leak(name.into().into_boxed_str());
    CUSTOM_PATTERNS.with(|cell| {
        cell.borrow_mut().push(NamedPattern {
            name: name_static,
            regex,
        });
    });
    Ok(())
}

/// Drop all custom patterns installed via [`register_custom_pattern`]
/// on the calling thread. Idempotent.
pub fn clear_custom_patterns() {
    CUSTOM_PATTERNS.with(|cell| cell.borrow_mut().clear());
}

/// Return the names of every default pattern, in catalog order.
pub fn default_pattern_names() -> Vec<&'static str> {
    DEFAULT_PATTERNS.iter().map(|p| p.name).collect()
}

/// Return the names of every custom pattern currently installed on the
/// calling thread.
pub fn custom_pattern_names() -> Vec<String> {
    CUSTOM_PATTERNS.with(|cell| cell.borrow().iter().map(|p| p.name.to_string()).collect())
}

/// Install a per-thread audit sink. The previous sink (if any) is
/// returned so callers can chain or restore.
pub fn install_audit_sink(sink: Option<AuditSink>) -> Option<AuditSink> {
    AUDIT_SINK.with(|cell| std::mem::replace(&mut *cell.borrow_mut(), sink))
}

fn emit_audit(events: &[RedactionEvent]) {
    if events.is_empty() {
        return;
    }
    // Always push to the per-thread ring so a synchronous
    // `drain_audit_ring` call returns every event recorded since
    // the last drain, regardless of whether an extra sink is
    // installed on this thread.
    AUDIT_RING.with(|ring| {
        let mut ring = ring.borrow_mut();
        for event in events {
            // Bounded cap: 1024 entries is well above any realistic
            // per-step audit pressure but small enough to be a
            // no-op for normal workloads and to keep a runaway
            // sink from OOMing the process.
            if ring.len() >= 1024 {
                ring.remove(0);
            }
            ring.push(event.clone());
        }
    });
    let sink = AUDIT_SINK.with(|cell| cell.borrow().clone());
    if let Some(sink) = sink {
        for event in events {
            sink(event);
        }
    }
}

/// Drain every audit event recorded on the calling thread since the
/// last drain. The returned vec is in the order events fired.
pub fn drain_audit_ring() -> Vec<RedactionEvent> {
    AUDIT_RING.with(|ring| std::mem::take(&mut *ring.borrow_mut()))
}

/// Clear the per-thread audit ring without returning its contents.
/// Used by `clear_policy_stack` so tests sharing a thread cannot
/// leak audit events into each other.
pub fn clear_audit_ring() {
    AUDIT_RING.with(|ring| ring.borrow_mut().clear());
}

/// Build the per-match replacement string in the canonical
/// `<redacted:<name>:<len>>` form. Length reflects the redacted match
/// in UTF-8 bytes.
fn replacement_for(name: &str, matched: &str) -> String {
    format!("<redacted:{name}:{}>", matched.len())
}

/// Replace any high-confidence secret matches in `input` with the
/// canonical `<redacted:<pattern_name>:<len>>` placeholder. Returns
/// `Cow::Borrowed` when nothing matched, so callers paying for a clone
/// only pay when there was real work.
///
/// The legacy `placeholder` argument is kept for callers that want a
/// flat `[redacted]` form (e.g. headers and URL params). When the
/// placeholder is the canonical `[redacted]` constant the named form
/// is used; any other placeholder is substituted verbatim so callers
/// that need a specific marker (URL-param escaping, etc.) still get
/// it byte-for-byte.
pub fn scan_secret_patterns<'a>(input: &'a str, placeholder: &str) -> Cow<'a, str> {
    if input.is_empty() {
        return Cow::Borrowed(input);
    }
    // Length cap is defense-in-depth against catastrophic regex
    // behavior. None of the default patterns have nested
    // quantifiers, but custom patterns can be arbitrary so the cap
    // keeps a malicious script from blocking the persistence path.
    if input.len() > MAX_SCAN_INPUT_BYTES {
        return Cow::Borrowed(input);
    }
    let use_named_placeholder = placeholder == crate::redact::REDACTED_PLACEHOLDER;

    let mut owned: Option<String> = None;
    let mut audit_events: BTreeMap<&'static str, RedactionEvent> = BTreeMap::new();

    // Drive defaults then custom patterns. We collect custom
    // patterns into a Vec so the closure does not borrow the
    // thread-local across the regex calls.
    let custom: Vec<NamedPattern> = CUSTOM_PATTERNS.with(|cell| cell.borrow().clone());
    let all_patterns = DEFAULT_PATTERNS.iter().chain(custom.iter());

    for pattern in all_patterns {
        let target: &str = owned.as_deref().unwrap_or(input);
        let matches: Vec<(usize, usize)> = pattern
            .regex
            .find_iter(target)
            .map(|m| (m.start(), m.end()))
            .collect();
        if matches.is_empty() {
            continue;
        }
        let total_bytes: usize = matches.iter().map(|(s, e)| e - s).sum();
        audit_events.insert(
            pattern.name,
            RedactionEvent {
                pattern_name: pattern.name.to_string(),
                match_count: matches.len(),
                bytes_redacted: total_bytes,
            },
        );

        // Walk matches in reverse so we can splice without
        // recomputing offsets after each cut.
        let mut buffer = target.to_string();
        for (start, end) in matches.into_iter().rev() {
            let matched_slice = &buffer[start..end];
            let replacement = if use_named_placeholder {
                replacement_for(pattern.name, matched_slice)
            } else {
                placeholder.to_string()
            };
            buffer.replace_range(start..end, &replacement);
        }
        owned = Some(buffer);
    }

    let result = match owned {
        Some(value) if value == input => Cow::Borrowed(input),
        Some(value) => Cow::Owned(value),
        None => Cow::Borrowed(input),
    };

    if matches!(result, Cow::Owned(_)) {
        let events: Vec<RedactionEvent> = audit_events.into_values().collect();
        emit_audit(&events);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_clean() {
        clear_custom_patterns();
        install_audit_sink(None);
        clear_audit_ring();
    }

    #[test]
    fn returns_borrowed_when_clean() {
        run_clean();
        let out = scan_secret_patterns("just plain text", crate::redact::REDACTED_PLACEHOLDER);
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn replaces_aws_and_github_tokens_with_named_placeholder() {
        run_clean();
        let input = "AKIAABCDEFGHIJKLMNOP and ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let out = scan_secret_patterns(input, crate::redact::REDACTED_PLACEHOLDER);
        let rendered = out.into_owned();
        assert!(rendered.contains("<redacted:aws_access_key:20>"));
        assert!(rendered.contains("<redacted:github_token:40>"));
        assert!(!rendered.contains("AKIAABCDEFGHIJKLMNOP"));
    }

    #[test]
    fn legacy_placeholder_path_still_works_for_url_param_values() {
        run_clean();
        let input = "AKIAABCDEFGHIJKLMNOP";
        // A non-`[redacted]` placeholder is used verbatim — this is
        // the URL-param escaping path.
        let out = scan_secret_patterns(input, "%5Bredacted%5D");
        assert!(out.contains("%5Bredacted%5D"));
        assert!(!out.contains("AKIAABCDEFGHIJKLMNOP"));
    }

    #[test]
    fn replaces_bearer_token_inside_text() {
        run_clean();
        let input = "header: Authorization: Bearer abcDEFghi123_-+/=xyz tail";
        let out = scan_secret_patterns(input, crate::redact::REDACTED_PLACEHOLDER);
        assert!(out.contains("<redacted:bearer_token:"));
        assert!(!out.contains("abcDEFghi123_-+/=xyz"));
        assert!(out.contains("tail"));
    }

    #[test]
    fn replaces_jwt_tokens() {
        run_clean();
        let input = "token=eyJabcd.eyJefgh.signature_pad here";
        let out = scan_secret_patterns(input, crate::redact::REDACTED_PLACEHOLDER);
        assert!(out.contains("<redacted:jwt:"));
        assert!(!out.contains("eyJabcd.eyJefgh.signature_pad"));
    }

    #[test]
    fn custom_pattern_redacts_and_is_introspectable() {
        run_clean();
        register_custom_pattern("acme_token", r"\bACME-[A-Z0-9]{8}\b").unwrap();
        assert_eq!(custom_pattern_names(), vec!["acme_token".to_string()]);
        let out = scan_secret_patterns(
            "header ACME-12345678 trailer",
            crate::redact::REDACTED_PLACEHOLDER,
        );
        assert!(
            out.contains("<redacted:acme_token:13>"),
            "expected acme_token redaction, got: {out}"
        );
        clear_custom_patterns();
        assert!(custom_pattern_names().is_empty());
    }

    #[test]
    fn audit_sink_receives_one_event_per_matching_pattern() {
        use std::cell::RefCell;
        use std::rc::Rc;
        run_clean();
        let captured: Rc<RefCell<Vec<RedactionEvent>>> = Rc::new(RefCell::new(Vec::new()));
        let sink_captured = captured.clone();
        install_audit_sink(Some(Rc::new(move |event| {
            sink_captured.borrow_mut().push(event.clone());
        })));
        let input =
            "AKIAABCDEFGHIJKLMNOP AKIA0000000000000000 ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let out = scan_secret_patterns(input, crate::redact::REDACTED_PLACEHOLDER);
        assert!(matches!(out, Cow::Owned(_)));
        let events = captured.borrow();
        assert_eq!(events.len(), 2);
        let by_name: BTreeMap<&str, &RedactionEvent> = events
            .iter()
            .map(|event| (event.pattern_name.as_str(), event))
            .collect();
        assert_eq!(by_name.get("aws_access_key").unwrap().match_count, 2);
        assert_eq!(by_name.get("github_token").unwrap().match_count, 1);
        // The synchronous ring captures the same events so a
        // compliance drain returns them regardless of which sink
        // (if any) is installed.
        drop(events);
        install_audit_sink(None);
        let ring = drain_audit_ring();
        assert_eq!(ring.len(), 2);
    }

    #[test]
    fn audit_ring_records_events_even_without_a_sink() {
        run_clean();
        let _ = scan_secret_patterns("AKIAABCDEFGHIJKLMNOP", crate::redact::REDACTED_PLACEHOLDER);
        let ring = drain_audit_ring();
        assert_eq!(ring.len(), 1);
        assert_eq!(ring[0].pattern_name, "aws_access_key");
        // Drain is destructive.
        assert!(drain_audit_ring().is_empty());
    }

    #[test]
    fn input_above_cap_is_passthrough() {
        run_clean();
        let huge = "AKIAABCDEFGHIJKLMNOP".repeat(MAX_SCAN_INPUT_BYTES / 20 + 1);
        let out = scan_secret_patterns(&huge, crate::redact::REDACTED_PLACEHOLDER);
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn default_pattern_names_are_stable() {
        let names = default_pattern_names();
        assert!(names.contains(&"jwt"));
        assert!(names.contains(&"github_token"));
        assert!(names.contains(&"github_pat_fine"));
        assert!(names.contains(&"slack_token"));
        assert!(names.contains(&"aws_access_key"));
        assert!(names.contains(&"bearer_token"));
    }
}
