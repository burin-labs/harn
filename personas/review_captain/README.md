# Review Captain Persona

This is the canonical Harn package for `review_captain`. Hosts such as
Burin Code should discover it through `harn persona list --json` and
`harn persona inspect review_captain --json`, then treat the resulting
manifest as Harn-owned metadata.

## Local Checks

```bash
cargo run --quiet --bin harn -- persona --manifest personas/review_captain/harn.toml inspect review_captain --json
cargo run --quiet --bin harn -- run personas/review_captain/manifest.harn
cargo run --quiet --bin harn -- eval personas/review_captain/evals/review_captain_smoke.json
```
