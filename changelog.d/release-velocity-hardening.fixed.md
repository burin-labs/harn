Hardened release audits against retry drag by keeping demo tests from copying
ignored `.harn` runtime state, printing release-audit lane log paths
immediately, narrowing the audit warm prebuild to the CLI binary, and reusing
one stable package-check target for extracted crate verification.
