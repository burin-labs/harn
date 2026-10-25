The consumer canary now waits out a GitHub rate limit, a server error or a dropped
connection, within a bounded retry, instead of ending the check unmeasured. Each read
that fails names its class (`api_access_denied`, `api_rate_limited` or
`api_unavailable`), the HTTP status, the token's step and the repository's label.
