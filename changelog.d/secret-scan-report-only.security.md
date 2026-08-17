Added a `Secret scan` CI workflow that runs gitleaks over the working tree on
every pull request, every push to `main`, and weekly. It is report-only for
now: `.gitleaks.toml` extends the default ruleset and allowlists nothing, and
the job carries `continue-on-error`, so findings are reported without blocking
a merge. The first scan returned 53 findings across 25 files, dominated by the
redaction and secret-scanning test corpora — files that hold secret-shaped
strings by design. Those are unaudited, so the gate stays non-blocking until
an audit lands and an allowlist is written from it.
