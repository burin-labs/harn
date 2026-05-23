/// Shared high-confidence secret pattern catalog used by both redaction
/// and the `secret_scan` builtin.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SecretPatternSpec {
    pub redaction_name: &'static str,
    pub detector: &'static str,
    pub source: &'static str,
    pub title: &'static str,
    pub regex: &'static str,
}

pub(crate) const DEFAULT_SECRET_PATTERN_SPECS: &[SecretPatternSpec] = &[
    SecretPatternSpec {
        redaction_name: "jwt",
        detector: "jwt-token",
        source: "harn-redaction",
        title: "JWT token",
        regex: r"\beyJ[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]{4,}\b",
    },
    SecretPatternSpec {
        redaction_name: "github_token",
        detector: "github-token",
        source: "gitleaks",
        title: "GitHub token",
        regex: r"\bgh[pousr]_[A-Za-z0-9]{36,255}\b",
    },
    SecretPatternSpec {
        redaction_name: "github_pat_fine",
        detector: "github-fine-grained-token",
        source: "gitleaks",
        title: "GitHub fine-grained personal access token",
        regex: r"\bgithub_pat_[A-Za-z0-9_]{20,255}\b",
    },
    SecretPatternSpec {
        redaction_name: "slack_token",
        detector: "slack-token",
        source: "trufflehog",
        title: "Slack token",
        regex: r"\bxox[abprs]-[A-Za-z0-9-]{10,255}\b",
    },
    SecretPatternSpec {
        redaction_name: "aws_access_key",
        detector: "aws-access-key-id",
        source: "gitleaks",
        title: "AWS access key id",
        regex: r"\b(?:AKIA|ASIA|AGPA|AIDA|ANPA|AROA|AIPA)[A-Z0-9]{16}\b",
    },
    SecretPatternSpec {
        redaction_name: "gitlab_token",
        detector: "gitlab-token",
        source: "detect-secrets",
        title: "GitLab personal access token",
        regex: r"\bglpat-[A-Za-z0-9_-]{20,255}\b",
    },
    SecretPatternSpec {
        redaction_name: "npm_token",
        detector: "npm-token",
        source: "detect-secrets",
        title: "npm access token",
        regex: r"\bnpm_[A-Za-z0-9]{36}\b",
    },
    SecretPatternSpec {
        redaction_name: "openai_key",
        detector: "openai-api-key",
        source: "detect-secrets",
        title: "OpenAI API key",
        regex: r"\bsk-[A-Za-z0-9_-]{20,255}\b",
    },
    SecretPatternSpec {
        redaction_name: "stripe_key",
        detector: "stripe-secret-key",
        source: "trufflehog",
        title: "Stripe secret or restricted key",
        regex: r"\b(?:rk|sk)_(?:live|test)_[0-9A-Za-z]{16,255}\b",
    },
    SecretPatternSpec {
        redaction_name: "private_key_block",
        detector: "private-key-block",
        source: "detect-secrets",
        title: "Private key block",
        regex: r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
    },
    SecretPatternSpec {
        redaction_name: "bearer_token",
        detector: "bearer-token",
        source: "harn-redaction",
        title: "Bearer token",
        regex: r"(?i)\bBearer\s+[A-Za-z0-9._\-+/=]{12,}",
    },
];
