/// Shared high-confidence secret pattern catalog used by both redaction
/// and the `secret_scan` builtin.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SecretPatternSpec {
    pub redaction_name: &'static str,
    pub detector: &'static str,
    pub source: &'static str,
    pub title: &'static str,
    pub regex: &'static str,
    /// How self-identifying the match is, so consumers can choose a policy per
    /// class instead of hard-coding detector names.
    ///
    /// `"high"` is a self-identifying TOKEN shape (provider prefix + charset +
    /// length, or a delimited key block) — safe to act on automatically, e.g. a
    /// hard-block exfil guard. `"heuristic"` is a KEYWORD/context match
    /// (`Bearer <b64>`, `password = "..."`) with higher recall and false
    /// positives — right for redaction (over-redaction is harmless) but NOT for
    /// hard-blocking legitimate edits/commands.
    pub precision: &'static str,
}

pub(crate) const PRECISION_HIGH: &str = "high";
pub(crate) const PRECISION_HEURISTIC: &str = "heuristic";

pub(crate) const DEFAULT_SECRET_PATTERN_SPECS: &[SecretPatternSpec] = &[
    SecretPatternSpec {
        redaction_name: "jwt",
        detector: "jwt-token",
        source: "harn-redaction",
        title: "JWT token",
        regex: r"\beyJ[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]{4,}\b",
        precision: PRECISION_HIGH,
    },
    SecretPatternSpec {
        redaction_name: "github_token",
        detector: "github-token",
        source: "gitleaks",
        title: "GitHub token",
        regex: r"\bgh[pousr]_[A-Za-z0-9]{36,255}\b",
        precision: PRECISION_HIGH,
    },
    SecretPatternSpec {
        redaction_name: "github_pat_fine",
        detector: "github-fine-grained-token",
        source: "gitleaks",
        title: "GitHub fine-grained personal access token",
        regex: r"\bgithub_pat_[A-Za-z0-9_]{20,255}\b",
        precision: PRECISION_HIGH,
    },
    SecretPatternSpec {
        redaction_name: "slack_token",
        detector: "slack-token",
        source: "trufflehog",
        title: "Slack token",
        regex: r"\bxox[abprs]-[A-Za-z0-9-]{10,255}\b",
        precision: PRECISION_HIGH,
    },
    SecretPatternSpec {
        redaction_name: "aws_access_key",
        detector: "aws-access-key-id",
        source: "gitleaks",
        title: "AWS access key id",
        regex: r"\b(?:AKIA|ASIA|AGPA|AIDA|ANPA|AROA|AIPA)[A-Z0-9]{16}\b",
        precision: PRECISION_HIGH,
    },
    SecretPatternSpec {
        redaction_name: "gitlab_token",
        detector: "gitlab-token",
        source: "detect-secrets",
        title: "GitLab personal access token",
        regex: r"\bglpat-[A-Za-z0-9_-]{20,255}\b",
        precision: PRECISION_HIGH,
    },
    SecretPatternSpec {
        redaction_name: "huggingface_token",
        detector: "huggingface-token",
        source: "huggingface-docs",
        title: "Hugging Face user access token",
        regex: r"\bhf_[A-Za-z0-9]{20,255}\b",
        precision: PRECISION_HIGH,
    },
    SecretPatternSpec {
        redaction_name: "cerebras_key",
        detector: "cerebras-api-key",
        source: "cerebras-docs",
        title: "Cerebras API key",
        regex: r"\bcsk-[A-Za-z0-9]{20,255}\b",
        precision: PRECISION_HIGH,
    },
    SecretPatternSpec {
        redaction_name: "together_key",
        detector: "together-api-key",
        source: "together-bug-report",
        title: "Together API key",
        regex: r"\btgp_v1_[A-Za-z0-9_-]{20,255}\b",
        precision: PRECISION_HIGH,
    },
    SecretPatternSpec {
        redaction_name: "google_api_key",
        detector: "google-api-key",
        source: "microsoft-purview",
        title: "Google API key",
        regex: r"\bAIza[A-Za-z0-9_-]{35}\b",
        precision: PRECISION_HIGH,
    },
    SecretPatternSpec {
        redaction_name: "npm_token",
        detector: "npm-token",
        source: "detect-secrets",
        title: "npm access token",
        regex: r"\bnpm_[A-Za-z0-9]{36}\b",
        precision: PRECISION_HIGH,
    },
    SecretPatternSpec {
        redaction_name: "openai_key",
        detector: "openai-api-key",
        source: "detect-secrets",
        title: "OpenAI API key",
        regex: r"\bsk-[A-Za-z0-9_-]{20,255}\b",
        precision: PRECISION_HIGH,
    },
    SecretPatternSpec {
        redaction_name: "stripe_key",
        detector: "stripe-secret-key",
        source: "trufflehog",
        title: "Stripe secret or restricted key",
        regex: r"\b(?:rk|sk)_(?:live|test)_[0-9A-Za-z]{16,255}\b",
        precision: PRECISION_HIGH,
    },
    SecretPatternSpec {
        redaction_name: "private_key_block",
        detector: "private-key-block",
        source: "detect-secrets",
        title: "Private key block",
        regex: r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
        precision: PRECISION_HIGH,
    },
    SecretPatternSpec {
        redaction_name: "bearer_token",
        detector: "bearer-token",
        source: "harn-redaction",
        title: "Bearer token",
        regex: r"(?i)\bBearer\s+[A-Za-z0-9._\-+/=]{12,}",
        precision: PRECISION_HEURISTIC,
    },
    SecretPatternSpec {
        redaction_name: "sensitive_assignment",
        detector: "sensitive-assignment",
        source: "detect-secrets-keyword-detector",
        title: "Sensitive key/value assignment",
        regex: r#"(?i)\b(?:api[_-]?key|access[_-]?token|refresh[_-]?token|id[_-]?token|client[_-]?secret|password|passwd|secret|token)\s*[:=]\s*(?:"[A-Za-z0-9._\-+/=]{6,}"|'[A-Za-z0-9._\-+/=]{6,}'|[A-Za-z0-9._\-+/=]*[0-9._\-+/=][A-Za-z0-9._\-+/=]*|secret|hidden|hideme|password|passwd|token)"#,
        precision: PRECISION_HEURISTIC,
    },
];
