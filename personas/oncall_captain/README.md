# Oncall Captain Persona

This is the canonical Harn package for `oncall_captain`. Hosts such as
Burin Code should discover it through `harn persona list --json` and
`harn persona inspect oncall_captain --json`, then treat the resulting
manifest as Harn-owned metadata.

## Local Checks

```bash
cargo run --quiet --bin harn -- persona --manifest personas/oncall_captain/harn.toml inspect oncall_captain --json
cargo run --quiet --bin harn -- run personas/oncall_captain/manifest.harn
cargo run --quiet --bin harn -- eval personas/oncall_captain/evals/oncall_captain_smoke.json
```
