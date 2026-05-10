//! Free-form string secret patterns reused for redaction.
//!
//! The [`crate::stdlib::secret_scan`] module defines the canonical set
//! of high-confidence secret regexes that the `secret_scan` builtin
//! reports to scripts. This module borrows that same set so a string
//! that scanning would flag is also a string that redaction will
//! scrub — there is one definition, not two.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;

struct Pattern {
    /// Regex against the full input.
    regex: Regex,
}

static SECRET_PATTERNS: LazyLock<Vec<Pattern>> = LazyLock::new(|| {
    vec![
        // AWS IAM-style access key ids (gitleaks).
        Pattern {
            regex: Regex::new(r"\b(?:AKIA|ASIA|AGPA|AIDA|ANPA|AROA|AIPA)[A-Z0-9]{16}\b").unwrap(),
        },
        // GitHub OAuth/PAT/server/refresh tokens.
        Pattern {
            regex: Regex::new(r"\bgh(?:p|o|u|s|r)_[A-Za-z0-9]{36,255}\b").unwrap(),
        },
        // GitHub fine-grained PAT.
        Pattern {
            regex: Regex::new(r"\bgithub_pat_[A-Za-z0-9_]{20,255}\b").unwrap(),
        },
        // GitLab personal access token.
        Pattern {
            regex: Regex::new(r"\bglpat-[A-Za-z0-9_-]{20,255}\b").unwrap(),
        },
        // npm access token.
        Pattern {
            regex: Regex::new(r"\bnpm_[A-Za-z0-9]{36}\b").unwrap(),
        },
        // OpenAI / Anthropic-style sk- keys (long, project, etc).
        Pattern {
            regex: Regex::new(r"\bsk-[A-Za-z0-9_-]{20,255}\b").unwrap(),
        },
        // Slack tokens.
        Pattern {
            regex: Regex::new(r"\bxox(?:a|b|p|r|s)-[A-Za-z0-9-]{10,255}\b").unwrap(),
        },
        // Stripe live/test keys.
        Pattern {
            regex: Regex::new(r"\b(?:rk|sk)_(?:live|test)_[0-9A-Za-z]{16,255}\b").unwrap(),
        },
        // Bearer tokens in free text.
        Pattern {
            regex: Regex::new(r"(?i)\bBearer\s+[A-Za-z0-9._\-+/=]{12,}").unwrap(),
        },
    ]
});

/// Replace any high-confidence secret matches in `input` with
/// `placeholder`. Returns `Cow::Borrowed` when nothing matched, so
/// callers paying for a clone only pay when there was real work.
pub fn scan_secret_patterns<'a>(input: &'a str, placeholder: &str) -> Cow<'a, str> {
    if input.is_empty() {
        return Cow::Borrowed(input);
    }
    let mut owned: Option<String> = None;
    for pattern in SECRET_PATTERNS.iter() {
        let target: &str = owned.as_deref().unwrap_or(input);
        if !pattern.regex.is_match(target) {
            continue;
        }
        let replaced = pattern.regex.replace_all(target, placeholder).into_owned();
        owned = Some(replaced);
    }
    if let Some(value) = owned {
        if value == input {
            Cow::Borrowed(input)
        } else {
            Cow::Owned(value)
        }
    } else {
        Cow::Borrowed(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_borrowed_when_clean() {
        let out = scan_secret_patterns("just plain text", "[redacted]");
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn replaces_aws_and_github_tokens() {
        let input = "AKIAABCDEFGHIJKLMNOP and ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let out = scan_secret_patterns(input, "[redacted]");
        assert!(out.contains("[redacted]"));
        assert!(!out.contains("AKIAABCDEFGHIJKLMNOP"));
        assert!(!out.contains("ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
    }

    #[test]
    fn replaces_bearer_token_inside_text() {
        let input = "header: Authorization: Bearer abcDEFghi123_-+/=xyz tail";
        let out = scan_secret_patterns(input, "[redacted]");
        assert!(out.contains("[redacted]"));
        assert!(!out.contains("Bearer abcDEFghi123_-+/=xyz"));
        assert!(out.contains("tail"));
    }
}
