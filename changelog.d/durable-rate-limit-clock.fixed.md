Durable rate-limit concurrency tests now pin mock time so slow release-audit
workers cannot expire the first queued request row mid-test.
