- An exhausted provider balance or hard spend limit (HTTP 429 with a billing
  code such as `insufficient_quota`, `billing_limit`, or
  `credit_balance_exhausted`) is no longer retried as a rate limit. Its thrown
  category is `generic` rather than the status's `rate_limit`, the agent
  loop's retry policy never retries `billing_limit`, and the agent terminal
  outcome carries the new `provider_billing` class instead of `rate_limited`,
  so embedders can tell a person the account needs attention instead of
  "try again".
