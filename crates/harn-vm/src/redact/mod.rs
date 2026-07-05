//! Unified redaction policy for persisted and rendered operational data.
//!
//! Harn writes transcripts, receipts, event logs, portal JSON, connector
//! status snapshots, and workflow artifacts. This module is the single source
//! of truth for scrubbing HTTP headers, URL query parameters, JSON tokens, and
//! free-form strings so the same
//! representative secret cannot leak through two surfaces by accident.
//!
//! # Categories
//!
//! - **Auth headers, cookies, signature/proxy tokens** — covered by
//!   [`RedactionPolicy::redact_headers`].
//! - **URLs with credentials in userinfo or sensitive query parameters**
//!   — covered by [`RedactionPolicy::redact_url`].
//! - **JSON fields whose name is auth/credential-shaped** — covered by
//!   [`RedactionPolicy::redact_json_in_place`].
//! - **Free-form strings carrying high-confidence secret patterns**
//!   (Stripe `sk_live_…`, GitHub `ghp_…`, AWS `AKIA…`, Bearer tokens,
//!   `-----BEGIN … PRIVATE KEY-----`) — covered by
//!   [`RedactionPolicy::redact_string`] and applied recursively by
//!   [`RedactionPolicy::redact_json_in_place`].
//!
//! # Host configuration
//!
//! Hosts compose policies via the builder methods (`with_safe_header`,
//! `with_extra_field`, `with_extra_url_param`, `disable_string_scan`).
//! Active policies are pushed onto a thread-local stack the same way
//! approval policies are, so a single orchestrator startup site can
//! install host overrides for every persistence path that calls
//! [`current_policy`].

mod manifest;
mod patterns;

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value as JsonValue;
use url::Url;

pub(crate) use manifest::json_path_child;
pub use manifest::{RedactionEntry, UnredactedSecret};
pub use patterns::{
    clear_audit_ring, clear_custom_patterns, custom_pattern_names, default_pattern_names,
    drain_audit_ring, install_audit_sink, register_custom_pattern, scan_secret_patterns, AuditSink,
    NamedPattern, RedactionEvent, TOKEN_REDACTION_AUDIT_TOPIC, TOKEN_REDACTION_DIAGNOSTIC,
};

/// Placeholder string used everywhere a redacted value would otherwise
/// appear. Kept as a single constant so portal CSS, downstream parsers,
/// and humans grepping logs can rely on one form.
pub const REDACTED_PLACEHOLDER: &str = "[redacted]";

/// Header value for redacted HTTP headers. Identical to
/// [`REDACTED_PLACEHOLDER`] today, exposed as a separate symbol so the
/// trigger/event tests that pre-date the unified module remain readable.
pub const REDACTED_HEADER_VALUE: &str = REDACTED_PLACEHOLDER;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedactionPolicy {
    safe_headers: BTreeSet<String>,
    deny_header_substrings: BTreeSet<String>,
    extra_deny_header_substrings: BTreeSet<String>,
    extra_field_names: BTreeSet<String>,
    extra_url_params: BTreeSet<String>,
    scan_strings: bool,
    redact_url_userinfo: bool,
}

impl Default for RedactionPolicy {
    fn default() -> Self {
        Self {
            safe_headers: default_safe_headers(),
            deny_header_substrings: default_deny_header_substrings(),
            extra_deny_header_substrings: BTreeSet::new(),
            extra_field_names: BTreeSet::new(),
            extra_url_params: BTreeSet::new(),
            scan_strings: true,
            redact_url_userinfo: true,
        }
    }
}

impl RedactionPolicy {
    /// Permissive policy used by tests that need raw data. No headers,
    /// fields, or strings are scrubbed.
    pub fn passthrough() -> Self {
        Self {
            safe_headers: BTreeSet::new(),
            deny_header_substrings: BTreeSet::new(),
            extra_deny_header_substrings: BTreeSet::new(),
            extra_field_names: BTreeSet::new(),
            extra_url_params: BTreeSet::new(),
            scan_strings: false,
            redact_url_userinfo: false,
        }
    }

    /// Add a header (case-insensitive) to the safe-list. Header
    /// redaction will leave its value untouched even if the name would
    /// otherwise look auth-shaped (e.g. an `x-…-key` header that is
    /// actually a request-id).
    pub fn with_safe_header(mut self, name: impl Into<String>) -> Self {
        self.safe_headers.insert(name.into().to_ascii_lowercase());
        self
    }

    /// Add a substring (case-insensitive) that always forces a header
    /// to be treated as sensitive. Useful for product-specific token
    /// header names that the default `cookie`/`authorization`/`token`/`secret`/`key`
    /// substring set would miss.
    pub fn with_deny_header_substring(mut self, fragment: impl Into<String>) -> Self {
        self.extra_deny_header_substrings
            .insert(fragment.into().to_ascii_lowercase());
        self
    }

    /// Add a JSON field name (case-insensitive, exact match) that should
    /// always be redacted regardless of value contents. Useful when a
    /// host knows it stores `internal_audit_token` or similar.
    pub fn with_extra_field(mut self, name: impl Into<String>) -> Self {
        self.extra_field_names
            .insert(name.into().to_ascii_lowercase());
        self
    }

    /// Add an extra URL query parameter name to redact.
    pub fn with_extra_url_param(mut self, name: impl Into<String>) -> Self {
        self.extra_url_params
            .insert(name.into().to_ascii_lowercase());
        self
    }

    /// Disable the heuristic free-form string scanner. The scanner adds
    /// a small but non-zero cost to every JSON payload walk; turn it off
    /// for performance-critical paths that have already been audited.
    pub fn disable_string_scan(mut self) -> Self {
        self.scan_strings = false;
        self
    }

    fn header_is_safe(&self, lower_name: &str) -> bool {
        // Exact-name allowlist is one source of truth in `safe_headers`;
        // suffix/substring rules below cover the families of debugging
        // headers that providers emit with arbitrary suffixes.
        if self.safe_headers.contains(lower_name) {
            return true;
        }
        lower_name.ends_with("-event")
            || lower_name.ends_with("-delivery")
            || lower_name.contains("timestamp")
            || lower_name.contains("request-id")
    }

    /// Whether a given HTTP header name should have its value replaced
    /// with [`REDACTED_HEADER_VALUE`].
    ///
    /// Host-explicit deny substrings always win, even over the built-in
    /// safe-list — that is how a host says "treat my own webhook
    /// delivery header as sensitive even though Harn would normally
    /// keep it for debugging."
    pub fn header_is_sensitive(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        if self
            .extra_deny_header_substrings
            .iter()
            .any(|fragment| lower.contains(fragment))
        {
            return true;
        }
        if self.header_is_safe(&lower) {
            return false;
        }
        self.deny_header_substrings
            .iter()
            .any(|fragment| lower.contains(fragment))
    }

    /// Whether a JSON object field name should be replaced with the
    /// redacted placeholder before the value is even inspected.
    pub fn field_is_sensitive(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        if self.extra_field_names.contains(&lower) {
            return true;
        }
        is_default_sensitive_field(&lower)
    }

    /// Whether a URL query parameter name should have its value
    /// replaced.
    pub fn url_param_is_sensitive(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        if self.extra_url_params.contains(&lower) {
            return true;
        }
        is_default_sensitive_url_param(&lower)
    }

    /// Returns a [`BTreeMap`] of headers with sensitive values replaced
    /// by [`REDACTED_HEADER_VALUE`].
    pub fn redact_headers(&self, headers: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        headers
            .iter()
            .map(|(name, value)| {
                if self.header_is_sensitive(name) {
                    (name.clone(), REDACTED_HEADER_VALUE.to_string())
                } else {
                    (name.clone(), value.clone())
                }
            })
            .collect()
    }

    /// Redact sensitive query parameters and credentials in URL
    /// userinfo. Returns the input unchanged if nothing matches or the
    /// URL fails to parse.
    pub fn redact_url(&self, url: &str) -> String {
        let Ok(mut parsed) = Url::parse(url) else {
            return self.redact_string(url).into_owned();
        };
        let mut changed = false;

        if self.redact_url_userinfo
            && (!parsed.username().is_empty() || parsed.password().is_some())
        {
            // url::Url returns Err only when the URL cannot have a
            // password (e.g. cannot-be-a-base). Treat that as a no-op.
            if parsed.set_username("").is_ok() {
                changed = true;
            }
            if parsed.set_password(None).is_ok() {
                changed = true;
            }
        }

        let pairs: Vec<(String, String)> = parsed
            .query_pairs()
            .map(|(key, value)| {
                if self.url_param_is_sensitive(&key) {
                    changed = true;
                    (key.into_owned(), REDACTED_PLACEHOLDER.to_string())
                } else {
                    (key.into_owned(), value.into_owned())
                }
            })
            .collect();
        let original_query = parsed.query().map(str::to_string);
        if !pairs.is_empty() {
            parsed.set_query(None);
            let mut query = parsed.query_pairs_mut();
            for (key, value) in &pairs {
                query.append_pair(key, value);
            }
        }
        // `query_pairs_mut` always re-encodes; restore the original
        // query string when nothing was actually redacted so we don't
        // perturb otherwise stable URLs.
        if !changed {
            parsed.set_query(original_query.as_deref());
            return parsed.to_string();
        }
        parsed.to_string()
    }

    /// Returns a redacted string. Cheap (`Cow::Borrowed`) when nothing
    /// matched. Applies, in order: URL-shaped string detection (so the
    /// userinfo or sensitive query params on `https://user:pw@…?api_key=…`
    /// are scrubbed), then high-confidence secret pattern replacement.
    pub fn redact_string<'a>(&self, value: &'a str) -> Cow<'a, str> {
        if !self.scan_strings {
            return Cow::Borrowed(value);
        }
        match self.redact_url_in_string(value) {
            Cow::Borrowed(_) => scan_secret_patterns(value, REDACTED_PLACEHOLDER),
            Cow::Owned(url_scrubbed) => {
                let pattern_scrubbed =
                    scan_secret_patterns(&url_scrubbed, REDACTED_PLACEHOLDER).into_owned();
                Cow::Owned(pattern_scrubbed)
            }
        }
    }

    /// Redact sensitive credentials and query parameters from HTTP(S) URLs
    /// embedded in free-form diagnostic text. This is intentionally separate
    /// from [`Self::redact_string`]: broad text tokenization is useful for
    /// transport errors that include URLs inside prose, while normal string
    /// redaction keeps its lower-perturbation standalone-URL behavior.
    pub fn redact_urls_in_text<'a>(&self, value: &'a str) -> Cow<'a, str> {
        let mut scan_cursor = 0;
        let mut emit_cursor = 0;
        let mut output: Option<String> = None;

        while let Some(relative_start) = find_http_url_start(&value[scan_cursor..]) {
            let start = scan_cursor + relative_start;
            let token_end = http_url_token_end(value, start);
            let token = &value[start..token_end];
            let Some((url, suffix)) = split_url_token(token) else {
                scan_cursor = token_end;
                continue;
            };
            let redacted = self.redact_url(url);
            if redacted != url {
                let output = output.get_or_insert_with(|| String::with_capacity(value.len()));
                output.push_str(&value[emit_cursor..start]);
                output.push_str(&redacted);
                output.push_str(suffix);
                emit_cursor = token_end;
            }
            scan_cursor = token_end;
        }

        match output {
            Some(mut output) => {
                output.push_str(&value[emit_cursor..]);
                Cow::Owned(output)
            }
            None => Cow::Borrowed(value),
        }
    }

    /// Conservative predicate for fields that must contain logical
    /// secret references rather than raw credential material.
    ///
    /// This is intentionally broader than [`redact_string`]: short
    /// fake-looking values such as `sk-live-secret` are useful test
    /// sentinels and should be rejected from `required_secrets` /
    /// context-pack manifests even though the free-form string
    /// redactor avoids replacing such short text globally.
    pub fn looks_like_secret_value(&self, value: &str) -> bool {
        let trimmed = value.trim();
        !trimmed.is_empty()
            && (self.redact_string(trimmed).as_ref() != trimmed
                || has_secret_prefix(trimmed)
                || is_long_bare_secret_candidate(trimmed))
    }

    /// If `value` is a single URL with credentials or sensitive query
    /// params, return the redacted form. Standalone URLs are common in
    /// logged request envelopes; we don't try to walk arbitrary text
    /// for embedded URLs because that turns into ad-hoc tokenization.
    fn redact_url_in_string<'a>(&self, value: &'a str) -> Cow<'a, str> {
        if !self.redact_url_userinfo
            || !(value.starts_with("http://") || value.starts_with("https://"))
        {
            return Cow::Borrowed(value);
        }
        let trimmed = value.trim();
        if trimmed.contains(char::is_whitespace) {
            return Cow::Borrowed(value);
        }
        let redacted = self.redact_url(trimmed);
        if redacted == trimmed {
            Cow::Borrowed(value)
        } else {
            Cow::Owned(redacted)
        }
    }

    /// Recursively walk a JSON value, redacting sensitive object fields
    /// and string contents in place.
    pub fn redact_json_in_place(&self, value: &mut JsonValue) {
        match value {
            JsonValue::Object(map) => {
                let mut keys_to_redact: Vec<String> = Vec::new();
                for (key, child) in map.iter_mut() {
                    if self.field_is_sensitive(key) {
                        keys_to_redact.push(key.clone());
                    } else {
                        self.redact_json_in_place(child);
                    }
                }
                for key in keys_to_redact {
                    map.insert(key, JsonValue::String(REDACTED_PLACEHOLDER.to_string()));
                }
            }
            JsonValue::Array(items) => {
                for item in items.iter_mut() {
                    self.redact_json_in_place(item);
                }
            }
            JsonValue::String(s) => {
                let redacted = self.redact_string(s);
                if let Cow::Owned(replacement) = redacted {
                    *s = replacement;
                }
            }
            _ => {}
        }
    }

    /// Convenience for callers that have an immutable JSON value: clone
    /// once and redact.
    pub fn redact_json(&self, value: &JsonValue) -> JsonValue {
        let mut clone = value.clone();
        self.redact_json_in_place(&mut clone);
        clone
    }
}

fn find_http_url_start(value: &str) -> Option<usize> {
    match (value.find("http://"), value.find("https://")) {
        (Some(http), Some(https)) => Some(http.min(https)),
        (Some(http), None) => Some(http),
        (None, Some(https)) => Some(https),
        (None, None) => None,
    }
}

fn http_url_token_end(value: &str, start: usize) -> usize {
    value[start..]
        .char_indices()
        .find_map(|(offset, character)| {
            (offset > 0 && is_url_text_delimiter(character)).then_some(start + offset)
        })
        .unwrap_or(value.len())
}

fn is_url_text_delimiter(character: char) -> bool {
    character.is_whitespace() || matches!(character, '"' | '\'' | '<' | '>' | '`')
}

fn split_url_token(token: &str) -> Option<(&str, &str)> {
    let mut prose_end = token.len();
    while prose_end > 0 {
        let candidate = &token[..prose_end];
        let last = candidate.chars().last()?;
        if !is_trailing_prose_punctuation(last) {
            break;
        }
        prose_end -= last.len_utf8();
    }
    if prose_end > 0 {
        let candidate = &token[..prose_end];
        if Url::parse(candidate).is_ok() {
            return Some((candidate, &token[prose_end..]));
        }
    }

    let mut end = token.len();
    while end > 0 {
        let candidate = &token[..end];
        if Url::parse(candidate).is_ok() {
            return Some((candidate, &token[end..]));
        }
        let last = candidate.chars().last()?;
        if !is_trailing_prose_punctuation(last) {
            return None;
        }
        end -= last.len_utf8();
    }
    None
}

fn is_trailing_prose_punctuation(character: char) -> bool {
    matches!(
        character,
        '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}'
    )
}

fn default_safe_headers() -> BTreeSet<String> {
    BTreeSet::from([
        "content-length".to_string(),
        "content-type".to_string(),
        "request-id".to_string(),
        "user-agent".to_string(),
        "x-a2a-delivery".to_string(),
        "x-correlation-id".to_string(),
        "x-github-delivery".to_string(),
        "x-github-event".to_string(),
        "x-github-hook-id".to_string(),
        "x-request-id".to_string(),
        "x-slack-request-timestamp".to_string(),
    ])
}

fn default_deny_header_substrings() -> BTreeSet<String> {
    BTreeSet::from([
        "authorization".to_string(),
        "cookie".to_string(),
        "secret".to_string(),
        "signature".to_string(),
        "token".to_string(),
        "key".to_string(),
    ])
}

fn is_default_sensitive_url_param(lower: &str) -> bool {
    let compact = compact_secret_name(lower);
    matches!(
        compact.as_str(),
        "apikey"
            | "accesstoken"
            | "refreshtoken"
            | "idtoken"
            | "clientsecret"
            | "password"
            | "secret"
            | "token"
            | "auth"
            | "bearer"
            | "sig"
            | "signature"
    ) || compact.ends_with("token")
        || compact.ends_with("secret")
        || compact.ends_with("password")
}

fn is_default_sensitive_field(lower: &str) -> bool {
    let compact = compact_secret_name(lower);
    matches!(
        compact.as_str(),
        "authorization"
            | "proxyauthorization"
            | "cookie"
            | "setcookie"
            | "apikey"
            | "xamzsecuritytoken"
            | "xapikey"
            | "xauthtoken"
            | "xcsrftoken"
            | "xxsrftoken"
            | "accesstoken"
            | "refreshtoken"
            | "idtoken"
            | "bearertoken"
            | "clientsecret"
            | "password"
            | "secret"
            | "passwd"
            | "privatekey"
            | "sessiontoken"
    ) || compact.ends_with("token")
        || compact.ends_with("secret")
        || compact.ends_with("password")
        || compact.ends_with("apikey")
}

fn compact_secret_name(lower: &str) -> String {
    lower
        .chars()
        .filter(|ch| *ch != '_' && *ch != '-')
        .collect()
}

fn has_secret_prefix(trimmed: &str) -> bool {
    trimmed.starts_with("sk-")
        || trimmed.starts_with("ghp_")
        || trimmed.starts_with("ghs_")
        || trimmed.starts_with("xoxb-")
        || trimmed.starts_with("xoxp-")
        || trimmed.starts_with("AKIA")
}

fn is_long_bare_secret_candidate(trimmed: &str) -> bool {
    trimmed.len() > 48
        && trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

thread_local! {
    static REDACTION_POLICY_STACK: RefCell<Vec<RedactionPolicy>> = const { RefCell::new(Vec::new()) };
}

/// Push a policy onto the thread-local stack. Pair every push with a
/// [`pop_policy`] call (or use [`PolicyGuard`]).
pub fn push_policy(policy: RedactionPolicy) {
    REDACTION_POLICY_STACK.with(|stack| stack.borrow_mut().push(policy));
}

/// Pop the most recently pushed policy. Safe to call when the stack is
/// empty.
pub fn pop_policy() {
    REDACTION_POLICY_STACK.with(|stack| {
        stack.borrow_mut().pop();
    });
}

/// Drop all installed policies, custom token-redaction patterns, the
/// audit sink, and the per-thread audit ring. Used by
/// `reset_thread_local_state` so test runs that share a thread cannot
/// leak policy overrides into each other.
pub fn clear_policy_stack() {
    REDACTION_POLICY_STACK.with(|stack| stack.borrow_mut().clear());
    patterns::clear_custom_patterns();
    let _ = patterns::install_audit_sink(None);
    patterns::clear_audit_ring();
}

/// Return the currently installed policy, falling back to
/// [`RedactionPolicy::default`] when the stack is empty. Always returns
/// an owned clone so callers can drop the borrow before recursing.
pub fn current_policy() -> RedactionPolicy {
    REDACTION_POLICY_STACK.with(|stack| {
        stack
            .borrow()
            .last()
            .cloned()
            .unwrap_or_else(RedactionPolicy::default)
    })
}

/// RAII guard that pushes a policy on construction and pops it on drop.
///
/// ```ignore
/// let _guard = harn_vm::redact::PolicyGuard::new(RedactionPolicy::default());
/// // … emit receipts, transcripts, etc.
/// ```
pub struct PolicyGuard;

impl PolicyGuard {
    pub fn new(policy: RedactionPolicy) -> Self {
        push_policy(policy);
        Self
    }
}

impl Drop for PolicyGuard {
    fn drop(&mut self) {
        pop_policy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_headers() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("Authorization".to_string(), "Bearer secret123".to_string()),
            ("Cookie".to_string(), "session=abc".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("X-Webhook-Token".to_string(), "tok-xyz".to_string()),
            (
                "X-Slack-Signature".to_string(),
                "v0=abcdef123456".to_string(),
            ),
            ("User-Agent".to_string(), "Harn/1.0".to_string()),
            ("X-GitHub-Delivery".to_string(), "delivery-123".to_string()),
        ])
    }

    #[test]
    fn default_policy_redacts_auth_headers_and_keeps_safe_ones() {
        let policy = RedactionPolicy::default();
        let redacted = policy.redact_headers(&sample_headers());
        assert_eq!(
            redacted.get("Authorization").unwrap(),
            REDACTED_HEADER_VALUE
        );
        assert_eq!(redacted.get("Cookie").unwrap(), REDACTED_HEADER_VALUE);
        assert_eq!(
            redacted.get("X-Webhook-Token").unwrap(),
            REDACTED_HEADER_VALUE
        );
        assert_eq!(
            redacted.get("X-Slack-Signature").unwrap(),
            REDACTED_HEADER_VALUE
        );
        assert_eq!(redacted.get("User-Agent").unwrap(), "Harn/1.0");
        assert_eq!(redacted.get("X-GitHub-Delivery").unwrap(), "delivery-123");
        assert_eq!(redacted.get("Content-Type").unwrap(), "application/json");
    }

    #[test]
    fn passthrough_policy_redacts_nothing() {
        let policy = RedactionPolicy::passthrough();
        let redacted = policy.redact_headers(&sample_headers());
        assert_eq!(redacted.get("Authorization").unwrap(), "Bearer secret123");
    }

    #[test]
    fn host_can_extend_safe_and_deny_headers() {
        let policy = RedactionPolicy::default()
            .with_safe_header("X-Webhook-Token")
            .with_deny_header_substring("delivery");
        let redacted = policy.redact_headers(&sample_headers());
        assert_eq!(redacted.get("X-Webhook-Token").unwrap(), "tok-xyz");
        assert_eq!(
            redacted.get("X-GitHub-Delivery").unwrap(),
            REDACTED_HEADER_VALUE,
            "host explicitly forced delivery to be sensitive"
        );
    }

    #[test]
    fn redact_url_strips_userinfo_and_sensitive_query_params() {
        let policy = RedactionPolicy::default();
        let redacted = policy.redact_url(
            "https://user:pw@api.example.com/v1?api_key=abcdef&clientSecret=hidden&page=2",
        );
        assert!(redacted.contains("api_key=%5Bredacted%5D"));
        assert!(redacted.contains("clientSecret=%5Bredacted%5D"));
        assert!(redacted.contains("page=2"));
        assert!(!redacted.contains("user:pw@"));
    }

    #[test]
    fn redact_url_leaves_clean_urls_alone() {
        let policy = RedactionPolicy::default();
        let url = "https://api.example.com/v1?page=2";
        assert_eq!(policy.redact_url(url), url);
    }

    #[test]
    fn redact_urls_in_text_strips_embedded_sensitive_urls() {
        let policy = RedactionPolicy::default();
        let redacted = policy.redact_urls_in_text(
            "clean https://status.example.com/health then \
             redirect from (https://user:pw@api.example.com/start?access_token=source-secret) \
             to http://public.example.com/next?client_secret=target-secret.",
        );
        assert!(redacted.starts_with("clean https://status.example.com/health then "));
        assert!(redacted.contains("access_token=%5Bredacted%5D"));
        assert!(redacted.contains("client_secret=%5Bredacted%5D"));
        assert!(!redacted.contains("source-secret"));
        assert!(!redacted.contains("target-secret"));
        assert!(!redacted.contains("user:pw@"));
        assert!(redacted.ends_with('.'));
    }

    #[test]
    fn redact_json_strips_sensitive_field_names_recursively() {
        let policy = RedactionPolicy::default();
        let mut value = json!({
            "headers": {
                "authorization": "Bearer abc",
                "X-Amz-Security-Token": "session",
                "x-trace-id": "trace_1",
            },
            "list": [
                { "auth_token": "tok_secret", "accessToken": "camel", "name": "alice" },
                { "name": "bob" },
            ],
            "clientSecret": "camel-secret",
            "free_form": "Bearer ghp_abcdefghijklmnopqrstuvwxyz0123456789ABCD",
            "url": "https://api.example.com/v1?api_key=hideme",
        });
        policy.redact_json_in_place(&mut value);
        assert_eq!(value["headers"]["authorization"], REDACTED_PLACEHOLDER);
        assert_eq!(
            value["headers"]["X-Amz-Security-Token"],
            REDACTED_PLACEHOLDER
        );
        assert_eq!(value["headers"]["x-trace-id"], "trace_1");
        assert_eq!(value["list"][0]["auth_token"], REDACTED_PLACEHOLDER);
        assert_eq!(value["list"][0]["accessToken"], REDACTED_PLACEHOLDER);
        assert_eq!(value["list"][0]["name"], "alice");
        assert_eq!(value["clientSecret"], REDACTED_PLACEHOLDER);
        let free_form = value["free_form"].as_str().unwrap();
        // Free-form pattern matches produce the OA-06 named placeholder
        // `<redacted:<pattern>:<len>>` so audit logs can attribute leaks to a
        // specific provider.
        assert!(
            free_form.contains("<redacted:"),
            "expected named placeholder, got: {free_form}"
        );
        assert!(!free_form.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789ABCD"));
    }

    #[test]
    fn policy_guard_pushes_and_pops_thread_local() {
        clear_policy_stack();
        assert_eq!(current_policy(), RedactionPolicy::default());
        {
            let policy = RedactionPolicy::default().with_extra_field("custom_token");
            let _guard = PolicyGuard::new(policy.clone());
            assert_eq!(current_policy(), policy);
        }
        assert_eq!(current_policy(), RedactionPolicy::default());
    }

    #[test]
    fn redact_string_replaces_known_secret_patterns() {
        let policy = RedactionPolicy::default();
        let input =
            "use sk-proj-abcdefghijklmnopqrstuvwxyz0123456789ABCD or AKIAABCDEFGHIJKLMNOP for now";
        let out = policy.redact_string(input);
        // Each provider pattern emits its own `<redacted:<name>:<len>>`
        // placeholder so audit logs can attribute the leak.
        assert!(out.contains("<redacted:openai_key:"));
        assert!(out.contains("<redacted:aws_access_key:"));
        assert!(!out.contains("AKIAABCDEFGHIJKLMNOP"));
        assert!(!out.contains("sk-proj-abcdefghijklmnopqrstuvwxyz0123456789ABCD"));
    }

    #[test]
    fn looks_like_secret_value_accepts_logical_secret_references() {
        let policy = RedactionPolicy::default();
        assert!(policy.looks_like_secret_value("sk-live-secret"));
        assert!(policy.looks_like_secret_value("AKIAABCDEFGHIJKLMNOP"));
        assert!(!policy.looks_like_secret_value("github/webhook-secret"));
        assert!(!policy.looks_like_secret_value("SPLUNK_READ_TOKEN"));
    }
}
