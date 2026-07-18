//! Free-form string secret patterns reused for redaction.
//!
//! Each pattern is named so the replacement placeholder is
//! `<redacted:<pattern_name>:<len>>` and audit events can attribute the
//! redaction to a specific provider. The shared
//! [`crate::secret_patterns`] catalog is also used by the
//! `secret_scan` builtin, so a string that scanning reports is also a
//! string that redaction scrubs.
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

use regex::Regex;

use crate::secret_patterns::compiled_default_secret_patterns;

/// Stable identifier emitted in audit logs for every token-redaction
/// event. Part of the OA-06 epic's compliance contract.
pub const TOKEN_REDACTION_DIAGNOSTIC: &str = "HARN-OAU-001";

/// Event-log topic used for token-redaction audit events.
pub const TOKEN_REDACTION_AUDIT_TOPIC: &str = "audit.token_redaction";

/// Size of a single regex scan window. Inputs at or below this size are
/// scanned in one pass; larger inputs are scanned in overlapping windows of
/// this size (see [`SCAN_WINDOW_OVERLAP_BYTES`]) so no single regex call ever
/// runs over more than this many bytes — keeping a pathological (custom)
/// pattern from triggering catastrophic behavior on the persistence hot path
/// — while a secret embedded anywhere in an oversized value is still redacted.
const MAX_SCAN_INPUT_BYTES: usize = 256 * 1024;

/// Overlap between consecutive scan windows for oversized inputs. Every window
/// re-scans this many bytes of its predecessor's tail so a secret straddling a
/// window boundary is fully contained in — and detected by — at least one
/// window. It must exceed the longest possible secret match; 8 KiB is far above
/// any real API key / token / PEM header while staying negligible relative to
/// the 256 KiB window, so the windowed scan stays linear in the input length.
const SCAN_WINDOW_OVERLAP_BYTES: usize = 8 * 1024;

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
    compiled_default_secret_patterns()
        .iter()
        .map(|pattern| pattern.spec.redaction_name)
        .collect()
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
    let use_named_placeholder = placeholder == crate::redact::REDACTED_PLACEHOLDER;

    // Oversized inputs are scanned in overlapping windows instead of being
    // passed through unredacted: a secret embedded in a large tool result,
    // transcript, or base64 blob under a non-sensitive field name must not leak
    // just because the whole value exceeds the single-pass window. No individual
    // regex call ever sees more than one window, so a pathological custom
    // pattern still cannot run over the entire giant string at once.
    if input.len() > MAX_SCAN_INPUT_BYTES {
        return scan_secret_patterns_windowed(input, use_named_placeholder, placeholder);
    }

    let mut owned: Option<String> = None;
    let mut audit_events: BTreeMap<&'static str, RedactionEvent> = BTreeMap::new();

    // Drive defaults then custom patterns. We collect custom
    // patterns into a Vec so the closure does not borrow the
    // thread-local across the regex calls.
    let custom: Vec<NamedPattern> = CUSTOM_PATTERNS.with(|cell| cell.borrow().clone());
    let all_patterns = compiled_default_secret_patterns()
        .iter()
        .map(|pattern| (pattern.spec.redaction_name, &pattern.regex))
        .chain(custom.iter().map(|pattern| (pattern.name, &pattern.regex)));

    for (pattern_name, regex) in all_patterns {
        let target: &str = owned.as_deref().unwrap_or(input);
        let matches: Vec<(usize, usize)> = regex
            .find_iter(target)
            .map(|m| (m.start(), m.end()))
            .collect();
        if matches.is_empty() {
            continue;
        }
        let total_bytes: usize = matches.iter().map(|(s, e)| e - s).sum();
        audit_events.insert(
            pattern_name,
            RedactionEvent {
                pattern_name: pattern_name.to_string(),
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
                replacement_for(pattern_name, matched_slice)
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

/// Round `offset` down to the nearest UTF-8 char boundary (or `0`).
fn floor_char_boundary(s: &str, mut offset: usize) -> usize {
    if offset >= s.len() {
        return s.len();
    }
    while offset > 0 && !s.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// Round `offset` up to the nearest UTF-8 char boundary (or `s.len()`).
fn ceil_char_boundary(s: &str, mut offset: usize) -> usize {
    if offset >= s.len() {
        return s.len();
    }
    while offset < s.len() && !s.is_char_boundary(offset) {
        offset += 1;
    }
    offset
}

/// Overlapping-window variant of [`scan_secret_patterns`] for inputs larger
/// than [`MAX_SCAN_INPUT_BYTES`].
///
/// Each pattern is scanned over consecutive windows of `MAX_SCAN_INPUT_BYTES`
/// bytes that overlap their predecessor by [`SCAN_WINDOW_OVERLAP_BYTES`]. Because
/// the overlap exceeds the longest possible secret, every match is *strictly
/// interior* to at least one window; a match that touches an artificial
/// (non-terminal) window edge is dropped there — which also discards the false
/// `\b` word boundaries that slicing would otherwise create — and picked up
/// whole in the neighbouring window. Global match ranges are collected in
/// pattern-priority order (earlier patterns win on overlap, matching the
/// single-pass path) and spliced once from the end so offsets stay valid.
fn scan_secret_patterns_windowed<'a>(
    input: &'a str,
    use_named_placeholder: bool,
    placeholder: &str,
) -> Cow<'a, str> {
    let step = MAX_SCAN_INPUT_BYTES - SCAN_WINDOW_OVERLAP_BYTES;
    let custom: Vec<NamedPattern> = CUSTOM_PATTERNS.with(|cell| cell.borrow().clone());

    // Claimed global byte ranges, kept sorted by start. Each range also carries
    // its replacement text and the pattern that produced it (for audit).
    struct Claim {
        start: usize,
        end: usize,
        replacement: String,
        pattern: &'static str,
    }
    let mut claims: Vec<Claim> = Vec::new();

    let all_patterns = compiled_default_secret_patterns()
        .iter()
        .map(|pattern| (pattern.spec.redaction_name, &pattern.regex))
        .chain(custom.iter().map(|pattern| (pattern.name, &pattern.regex)));
    for (pattern_name, regex) in all_patterns {
        let mut window_start = 0usize;
        loop {
            let ws = floor_char_boundary(input, window_start);
            let we = ceil_char_boundary(
                input,
                (window_start + MAX_SCAN_INPUT_BYTES).min(input.len()),
            );
            for m in regex.find_iter(&input[ws..we]) {
                let gs = ws + m.start();
                let ge = ws + m.end();
                // Drop matches touching an artificial window edge; the same
                // secret is strictly interior to the neighbouring window.
                if (gs == ws && ws != 0) || (ge == we && we != input.len()) {
                    continue;
                }
                // First (highest-priority) pattern wins on overlap; this also
                // deduplicates a match seen in two overlapping windows.
                if claims.iter().any(|c| gs < c.end && c.start < ge) {
                    continue;
                }
                let replacement = if use_named_placeholder {
                    replacement_for(pattern_name, &input[gs..ge])
                } else {
                    placeholder.to_string()
                };
                claims.push(Claim {
                    start: gs,
                    end: ge,
                    replacement,
                    pattern: pattern_name,
                });
            }
            if we >= input.len() {
                break;
            }
            window_start += step;
        }
    }

    if claims.is_empty() {
        return Cow::Borrowed(input);
    }

    claims.sort_by_key(|c| c.start);

    // Audit tallies, grouped by pattern in catalog order.
    let mut audit_events: BTreeMap<&'static str, RedactionEvent> = BTreeMap::new();
    for claim in &claims {
        let event = audit_events
            .entry(claim.pattern)
            .or_insert_with(|| RedactionEvent {
                pattern_name: claim.pattern.to_string(),
                match_count: 0,
                bytes_redacted: 0,
            });
        event.match_count += 1;
        event.bytes_redacted += claim.end - claim.start;
    }

    let mut out = input.to_string();
    for claim in claims.iter().rev() {
        out.replace_range(claim.start..claim.end, &claim.replacement);
    }

    emit_audit(&audit_events.into_values().collect::<Vec<_>>());
    Cow::Owned(out)
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
    fn replaces_sensitive_assignments_inside_text() {
        run_clean();
        let input = "retry with token=abc123 and max_tokens=200";
        let out = scan_secret_patterns(input, crate::redact::REDACTED_PLACEHOLDER);
        assert!(out.contains("<redacted:sensitive_assignment:"));
        assert!(!out.contains("token=abc123"));
        assert!(out.contains("max_tokens=200"));
    }

    #[test]
    fn sensitive_assignment_preserves_source_declarations() {
        run_clean();
        let input = "pub const Token = struct { kind: u8 };\nconst Secret = enum { a, b };";
        let out = scan_secret_patterns(input, crate::redact::REDACTED_PLACEHOLDER);
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn sensitive_assignment_redacts_placeholder_secret_words() {
        run_clean();
        let input = "Checkout incident needed the same query token=secret";
        let out = scan_secret_patterns(input, crate::redact::REDACTED_PLACEHOLDER);
        assert!(out.contains("<redacted:sensitive_assignment:"));
        assert!(!out.contains("token=secret"));
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
    fn replaces_private_key_blocks() {
        run_clean();
        let input =
            "-----BEGIN OPENSSH PRIVATE KEY-----\nsecret-material\n-----END OPENSSH PRIVATE KEY-----";
        let out = scan_secret_patterns(input, crate::redact::REDACTED_PLACEHOLDER);
        assert!(out.contains("<redacted:private_key_block:"));
        assert!(!out.contains("secret-material"));
    }

    #[test]
    fn replaces_ai_provider_tokens() {
        run_clean();
        let huggingface = format!("hf_{}", "a".repeat(24));
        let cerebras = format!("csk-{}", "b".repeat(48));
        let together = format!("tgp_v1_{}", "c".repeat(32));
        let google = format!("AIza{}", "D".repeat(35));
        let input = format!("{huggingface} {cerebras} {together} {google}");

        let out = scan_secret_patterns(&input, crate::redact::REDACTED_PLACEHOLDER);
        let rendered = out.into_owned();

        assert!(rendered.contains("<redacted:huggingface_token:"));
        assert!(rendered.contains("<redacted:cerebras_key:"));
        assert!(rendered.contains("<redacted:together_key:"));
        assert!(rendered.contains("<redacted:google_api_key:"));
        assert!(!rendered.contains(&huggingface));
        assert!(!rendered.contains(&cerebras));
        assert!(!rendered.contains(&together));
        assert!(!rendered.contains(&google));
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

    const AWS_KEY: &str = "AKIAABCDEFGHIJKLMNOP";

    #[test]
    fn secret_past_the_scan_cap_is_redacted() {
        // A secret placed well beyond MAX_SCAN_INPUT_BYTES must still be scrubbed
        // — the old behavior passed the whole value through unredacted.
        run_clean();
        let mut input = " ".repeat(MAX_SCAN_INPUT_BYTES + 4096);
        input.push_str(AWS_KEY);
        input.push(' ');
        assert!(input.len() > MAX_SCAN_INPUT_BYTES);
        let out = scan_secret_patterns(&input, crate::redact::REDACTED_PLACEHOLDER);
        assert!(matches!(out, Cow::Owned(_)), "oversized secret must redact");
        assert!(
            !out.contains(AWS_KEY),
            "secret leaked: {}",
            &out[out.len().saturating_sub(64)..]
        );
        assert!(out.contains("<redacted:aws_access_key:20>"));
    }

    #[test]
    fn secret_straddling_a_window_boundary_is_redacted() {
        // Place the 20-byte key so it spans the first window's end
        // (MAX_SCAN_INPUT_BYTES): half inside window 0, half beyond. The overlap
        // guarantees it is fully interior to window 1 and thus detected.
        run_clean();
        let prefix_len = MAX_SCAN_INPUT_BYTES - (AWS_KEY.len() / 2);
        let mut input = " ".repeat(prefix_len);
        input.push_str(AWS_KEY);
        input.push_str(&" ".repeat(SCAN_WINDOW_OVERLAP_BYTES)); // ensure a 2nd window exists
        let out = scan_secret_patterns(&input, crate::redact::REDACTED_PLACEHOLDER);
        assert!(!out.contains(AWS_KEY), "straddling secret leaked");
        assert!(out.contains("<redacted:aws_access_key:20>"));
        // Exactly one redaction — the overlap must not double-count it.
        assert_eq!(out.matches("<redacted:aws_access_key:20>").count(), 1);
    }

    #[test]
    fn oversized_non_secret_blob_is_not_over_redacted() {
        // A large innocuous value (no secret) must pass through untouched, not be
        // blanket-redacted.
        run_clean();
        let blob = "lorem ipsum dolor sit amet ".repeat(MAX_SCAN_INPUT_BYTES / 20);
        assert!(blob.len() > MAX_SCAN_INPUT_BYTES);
        let out = scan_secret_patterns(&blob, crate::redact::REDACTED_PLACEHOLDER);
        assert!(
            matches!(out, Cow::Borrowed(_)),
            "clean blob must not be rewritten"
        );
        assert_eq!(out.as_ref(), blob);
    }

    #[test]
    fn oversized_scan_records_audit_event() {
        run_clean();
        let mut input = " ".repeat(MAX_SCAN_INPUT_BYTES + 100);
        input.push_str(AWS_KEY);
        input.push(' ');
        let _ = scan_secret_patterns(&input, crate::redact::REDACTED_PLACEHOLDER);
        let ring = drain_audit_ring();
        assert_eq!(ring.len(), 1);
        assert_eq!(ring[0].pattern_name, "aws_access_key");
        assert_eq!(ring[0].match_count, 1);
        assert_eq!(ring[0].bytes_redacted, 20);
    }

    #[test]
    fn multi_megabyte_scan_stays_linear_and_redacts() {
        // ~5 MiB (≈20 windows) with a single embedded secret. The windowed scan
        // is linear in the input, so this returns near-instantly; a catastrophic
        // (e.g. O(n²)) regression would instead blow the test-runner timeout. We
        // assert on the result rather than the wall clock to stay deterministic.
        run_clean();
        let mut input = "x ".repeat(5 * 1024 * 1024 / 2);
        input.push_str(AWS_KEY);
        input.push(' ');
        let out = scan_secret_patterns(&input, crate::redact::REDACTED_PLACEHOLDER);
        assert!(!out.contains(AWS_KEY));
        assert_eq!(out.matches("<redacted:aws_access_key:20>").count(), 1);
    }

    #[test]
    fn default_pattern_names_are_stable() {
        let names = default_pattern_names();
        assert!(names.contains(&"jwt"));
        assert!(names.contains(&"github_token"));
        assert!(names.contains(&"github_pat_fine"));
        assert!(names.contains(&"slack_token"));
        assert!(names.contains(&"aws_access_key"));
        assert!(names.contains(&"huggingface_token"));
        assert!(names.contains(&"cerebras_key"));
        assert!(names.contains(&"together_key"));
        assert!(names.contains(&"google_api_key"));
        assert!(names.contains(&"private_key_block"));
        assert!(names.contains(&"bearer_token"));
        assert!(names.contains(&"sensitive_assignment"));
    }
}
