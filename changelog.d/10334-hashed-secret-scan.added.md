`secret_scan(content, hashes)` checks complete, case-insensitive tokens against
a nonempty SHA-256 denylist. It returns typed findings with source locations and
digest-only redaction. Native and portable execution share the scanner. The
one-argument credential-pattern scan keeps its existing behavior.
