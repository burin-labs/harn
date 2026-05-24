# Skill provenance

Harn can require cryptographic provenance for filesystem-backed skills
before `load_skill(...)` promotes their bodies into an agent session.
The design is intentionally small:

- Ed25519 keys, generated locally.
- Detached JSON signature chains written next to `SKILL.md`.
- A flat trusted-signer directory at `~/.harn/trusted-signers/`
  for both authors and endorsers.
- Optional registry lookup for `<fingerprint>.pub` when a project wants
  to resolve trusted signers from a shared location instead of HOME.

This mirrors the "verify a detached signature against a trusted
signer set" workflow used by tools like `npm audit signatures` and
Sigstore's `cosign`, but without transparency logs or PKI yet.

## Threat model

Skill provenance is aimed at one narrow problem: a model should not
silently load arbitrary prompt instructions from disk when a project
or caller requires signed skills. A trusted skill needs an author
signature plus at least one endorsement signature from a separate
co-signer or auditor.

It helps with:

- Accidental edits to a trusted skill bundle.
- Local or package-level tampering with `SKILL.md`.
- Distinguishing "signed but unknown signer" from "valid trusted signer".
- Surfacing who authored and who endorsed a skill as Harn-visible policy
  input for agents.
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

Add an endorsement from a separate co-signer or auditor key:

```bash
harn skill endorse .harn/skills/deploy/SKILL.md --key ~/.harn/keys/auditor.pem
```

That writes `.harn/skills/deploy/SKILL.md.sig` with this shape:

```json
{
  "schema": "harn-skill-sig/v2",
  "signed_at": "2026-04-19T23:56:56.325809Z",
  "signer_fingerprint": "<sha256-hex>",
  "ed25519_sig_base64": "<detached-signature>",
  "skill_sha256": "<sha256-hex>",
  "endorsements": [
    {
      "signed_at": "2026-04-20T01:12:41.105813Z",
      "endorser_fingerprint": "<sha256-hex>",
      "ed25519_sig_base64": "<detached-signature>"
    }
  ]
}
```

Verify a skill:

```bash
harn skill verify .harn/skills/deploy/SKILL.md
```

Verification succeeds only when:

1. `SKILL.md.sig` exists and matches the `harn-skill-sig/v2` schema.
2. `skill_sha256` matches the current `SKILL.md` contents.
3. The author and endorsement public keys resolve from either:
   - `~/.harn/trusted-signers/<fingerprint>.pub`, or
   - the configured signer registry URL.
4. The author Ed25519 signature and every endorsement signature validate.
5. At least one endorsement is present and trusted.
6. If the skill declares `trusted_signers`, the author fingerprint is in
   that allowlist too.
7. If the skill declares `trusted_endorsers`, every endorsement
   fingerprint is in that allowlist too.

Inspect the signature chain and signer trust scores:

```bash
harn skill who-signed .harn/skills/deploy/SKILL.md
harn skill who-signed .harn/skills/deploy/SKILL.md --json
```

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
trusted_endorsers:
  - 78a4601fafb260a8238a6e8b7b1b39e6c0959d9d2a68c7a8d9d245d1dc5a1d35
```

When enforcement is active:

- missing signatures, invalid signatures, unknown signers, disallowed
  signers, or missing endorsements keep the skill out of the startup
  registry or block a direct `load_skill(...)` call with
  `UnsignedSkillError`
- the attached provenance metadata carries the precise status, such as
  `missing_signature`, `invalid_signature`, `missing_signer`,
  `untrusted_signer`, or `missing_endorsement`

Unsigned skills still load by default unless one of those policies is
enabled. User and system layer skills are stricter: a failed provenance
check is enough to omit the entry by default, though a completely
unsigned skill can still be listed. For listed filesystem-backed skills,
executable frontmatter is only surfaced when the attached provenance
verifies as trusted. Untrusted, unsigned, or tampered skills omit
`hooks`, `command`, and `run` fields from the runtime registry.

## Trust records and OpenTrustGraph

Every runtime `load_skill(...)` attempt emits a transcript trust record
with `kind == "skill.loaded"` and metadata:

```json
{
  "skill_id": "deploy",
  "signer_fingerprint": "<sha256-hex or null>",
  "signed": true,
  "trusted": true,
  "author": {
    "fingerprint": "<sha256-hex>",
    "trust_actor_id": "<sha256-hex>",
    "trust_action": "skill.provenance"
  },
  "endorsements": [
    {
      "fingerprint": "<sha256-hex>",
      "trust_actor_id": "<sha256-hex>",
      "trust_action": "skill.provenance",
      "trusted": true,
      "status": "verified"
    }
  ],
  "trust_policy_input": {
    "action": "skill.provenance",
    "author_actor_id": "<sha256-hex>",
    "endorser_actor_ids": ["<sha256-hex>"]
  }
}
```

That record is the current OpenTrustGraph integration point in Harn:
the agent transcript carries an explicit provenance edge every time a
skill body is promoted into the prompt. Downstream consumers can ingest
those records into a larger trust graph without re-verifying the skill
load from scratch.

Harn code can query the same chain from the active skill registry:

```harn
let chain = skill_who_signed(skills, "deploy")
let trust_inputs = chain.trust_policy_input
let signer_history = trust.query({
  actor: trust_inputs.author_actor_id,
  action: trust_inputs.action,
})
```
