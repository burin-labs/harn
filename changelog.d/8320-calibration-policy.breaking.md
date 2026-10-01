- Reviewer calibration accepts a `--policy` overlay on the running runtime's
  bundled policy and records the effective policy fingerprint. `--diagnostics`
  adds the reviewer trace, and each case retains the reviewer's failure detail.
  `approval_review_policy` returns a typed resolved policy; unknown keys and
  invalid field values fail before a reviewer call.

  Migration: remove unsupported overlay keys and give each field its declared
  type. Caller wording belongs in `host_guidance`; an explicit route belongs in
  `reviewer.provider` and `reviewer.model`. Omitted fields retain bundled values.
  Rust struct literals for `ApprovalReviewPolicy` must add `host_guidance: None`,
  and literals for `ReviewerConfig` must add `provider: None` unless overriding
  those values.
