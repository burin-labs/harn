- **Release publish retry.** The publish script now waits for its streaming log
  capture before classifying retryable cargo publish errors, so release-audit
  tests do not miss fallback-triggering output on Linux CI.
