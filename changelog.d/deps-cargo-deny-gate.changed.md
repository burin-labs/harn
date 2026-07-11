- Add a `cargo-deny` supply-chain gate (`deny.toml` + a `Supply chain` CI
  workflow) covering security advisories, license posture, and dependency
  sourcing across the whole Rust workspace. Licenses/bans/sources block; the
  RustSec advisory scan is advisory-only so a newly-published advisory can't red
  an unrelated PR.
