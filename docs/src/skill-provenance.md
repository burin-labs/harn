# Skill provenance

Harn can require cryptographic provenance for filesystem-backed skills
before `load_skill(...)` promotes their bodies into an agent session.
The design is intentionally small:

- Ed25519 keys, generated locally.
- Detached JSON signatures written next to `SKILL.md`.
- A flat trusted-signer directory at `~/.harn/trusted-signers/`.
- Optional registry lookup for `<fingerprint>.pub` when a project wants
  to resolve trusted signers from a shared location instead of HOME.

This mirrors the "verify a detached signature against a trusted
signer set" workflow used by tools like `npm audit signatures` and
Sigstore's `cosign`, but without transparency logs or PKI yet.

## Threat model

Skill provenance is aimed at one narrow problem: a model should not
silently load arbitrary prompt instructions from disk when a project
or caller requires signed skills.

It helps with:

- Accidental edits to a trusted skill bundle.
- Local or package-level tampering with `SKILL.md`.
- Distinguishing "signed but unknown signer" from "valid trusted signer".
- Emitting a durable audit record for every skill load attempt.

It does **not** currently provide:

- Certificate chains or organizational identities.
- Transparency logs, Rekor-style inclusion proofs, or timestamp
  witnesses.
- Revocation beyond removing a signer from the trusted registry.
- Integrity for non-`SKILL.md` bundled files.

Today the signed payload is the exact `SKILL.md` byte stream. If a
skill's bundled `files/` content also needs integrity guarantees, that
should be added as a future manifest hash-set rather than hand-waving
that protection into the current design.

## Key generation

Generate a new Ed25519 keypair:

```bash
harn skill key generate --out ~/.harn/keys/release-signer.pem
```

This writes:

- `~/.harn/keys/release-signer.pem` — private key PEM
- `~/.harn/keys/release-signer.pem.pub` — public key PEM

The command prints the signer's SHA-256 fingerprint. Harn fingerprints
the raw Ed25519 public key bytes and uses that hex digest as the trust
identifier everywhere else.

## Sign and verify

Sign a skill manifest:

```bash
harn skill sign .harn/skills/deploy/SKILL.md --key ~/.harn/keys/release-signer.pem
```

That writes `.harn/skills/deploy/SKILL.md.sig` with this shape:

```json
{
  "schema": "harn-skill-sig/v1",
  "signed_at": "2026-04-19T23:56:56.325809Z",
  "signer_fingerprint": "<sha256-hex>",
  "ed25519_sig_base64": "<detached-signature>",
  "skill_sha256": "<sha256-hex>"
}
```

Verify a skill:

```bash
harn skill verify .harn/skills/deploy/SKILL.md
```

Verification succeeds only when:

1. `SKILL.md.sig` exists and matches the `harn-skill-sig/v1` schema.
2. `skill_sha256` matches the current `SKILL.md` contents.
3. The signer's public key resolves from either:
   - `~/.harn/trusted-signers/<fingerprint>.pub`, or
   - the configured signer registry URL.
4. The Ed25519 signature validates.
5. If the skill declares `trusted_signers`, the signer fingerprint is in
   that allowlist too.

## Trusted signer management

The local trust store is a flat directory:

```text
~/.harn/trusted-signers/<fingerprint>.pub
```

Add a signer from a file or URL:

```bash
harn skill trust add --from ~/.harn/keys/release-signer.pem.pub
harn skill trust add --from https://skills.example.com/signers/<fingerprint>.pub
```

List locally trusted signers:

```bash
harn skill trust list
```

Projects can also configure a shared registry base URL in `harn.toml`:

```toml
[skills]
signer_registry_url = "./signers"
```

Relative values are resolved against the directory that holds
`harn.toml`. Harn looks up `<signer_registry_url>/<fingerprint>.pub`.

## Enforcing signed loads

There are three ways to require signed skills:

- Per call:

```json
load_skill({ name: "deploy", require_signature: true })
```

- Global environment:

```bash
HARN_REQUIRE_SIGNED_SKILLS=1 harn run main.harn
```

- Per skill policy in `SKILL.md` frontmatter:

```yaml
require_signature: true
trusted_signers:
  - 02488db042c9242ac7cd3554f8bc47099a3e1ff3f0d696906b57678f976f6064
```

When enforcement is active:

- missing or invalid signatures fail with `UnsignedSkillError`
- valid signatures from unknown or disallowed signers fail with
  `UntrustedSignerError`

Unsigned skills still load by default unless one of those policies is
enabled.

## Trust records and OpenTrustGraph

Every runtime `load_skill(...)` attempt emits a transcript trust record
with `kind == "skill.loaded"` and metadata:

```json
{
  "skill_id": "deploy",
  "signer_fingerprint": "<sha256-hex or null>",
  "signed": true,
  "trusted": true
}
```

That record is the current OpenTrustGraph integration point in Harn:
the agent transcript carries an explicit provenance edge every time a
skill body is promoted into the prompt. Downstream consumers can ingest
those records into a larger trust graph without re-verifying the skill
load from scratch.
