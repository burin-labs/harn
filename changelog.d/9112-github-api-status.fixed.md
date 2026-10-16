CI retries bounded GitHub reads when the CLI reports a bare HTTP 502, 503, or
504 status, while other failures still stop immediately.
