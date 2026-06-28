# Python verifier (reference, non-Harn)

`verify_chain.py` is a self-contained `opentrustgraph-chain/v0`
verifier written in pure Python (no third-party dependencies). It
exists as a portable proof point that the OpenTrustGraph hash and
linkage contract is interoperable with non-Harn runtimes.

```bash
# Verify a chain export.
python3 verify_chain.py ../../fixtures/valid/decision-chain.json
python3 verify_chain.py ../../fixtures/valid/tier-transition.json
python3 verify_chain.py ../../fixtures/valid/effect-inheritance-chain.json

# Reject tampered or missing-approval fixtures.
python3 verify_chain.py ../../fixtures/invalid/tampered-chain.json
python3 verify_chain.py ../../fixtures/invalid/missing-approval.json
python3 verify_chain.py ../../fixtures/invalid/actor-chain-parentage.json

# Read from stdin (for piping out of a producer):
harn trust export | python3 verify_chain.py
```

The script implements the canonicalization rule documented in
[`../../CONFORMANCE.md`](../../CONFORMANCE.md): remove `entry_hash`,
sort object keys lexicographically at every nesting level, emit JSON
with no insignificant whitespace, and SHA-256 the resulting bytes.

When extending it (e.g. adding Protobuf-to-JSON transcode), keep
behaviour byte-identical to the Harn reference impl — the Harn unit
tests in `crates/harn-vm/src/trust_graph.rs` will catch a regression
because they exercise the same fixtures.
