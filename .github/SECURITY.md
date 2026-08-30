# Security policy

## Reporting a vulnerability

Email **security@harn.cloud** with the details. Encrypt with our public key
if the report contains exploit material (key available on request).

Please include:

- a clear description of the issue and the impact (e.g. sandbox escape from a
  `.harn` script, secret exfiltration through a capability or connector,
  arbitrary code execution in `harn run`, signature forgery on a `harn pack`
  artifact)
- a minimal reproduction, ideally a `.harn` script or a `harn` CLI invocation
- the affected `harn` version (`harn --version`) and platform
- whether the issue has been disclosed publicly or to other parties

## Response window

We aim to:

- acknowledge new reports within **2 business days**
- triage and confirm (or dispute) within **5 business days**
- ship a fix or mitigation within **30 days** for confirmed issues, faster for
  actively-exploited or sandbox-escape bugs

## Scope

In scope:

- the Harn VM sandbox: filesystem roots, network egress policy, process
  spawning, and any path that lets a script reach state the capability policy
  denied it
- capability policy and the secret-redaction machinery, including any leak of
  credential material into transcripts, logs, or child processes
- `harn pack` signing and verification, and the package/lockfile resolution
  path
- `harn serve` and the protocol surfaces it exposes (ACP, A2A, MCP)
- release artifacts and the install script, including checksum or signature
  mismatches that would let an attacker ship unintended code

Out of scope for *this* repository:

- vulnerabilities in a downstream host product or Harn Cloud. Still report them,
  to the same address — **security@harn.cloud** — naming the product. Do not
  open a public issue against them.
- a script that is merely granted broad capabilities by its author. Harn
  enforces the policy it is given; a script explicitly granted process and
  network access is behaving as configured, not escaping the sandbox.

## Coordinated disclosure

We support coordinated disclosure. Please give us the response window above
before publishing details. We will credit reporters in the release notes for
the fix unless asked otherwise.
