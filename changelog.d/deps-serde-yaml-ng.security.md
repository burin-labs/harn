- Replace the unmaintained, unsound `serde_yml` YAML crate
  (RUSTSEC-2025-0068) with the maintained `serde_yaml_ng` fork across
  `harn-cli`, `harn-serve`, and `harn-vm`. Same `unsafe-libyaml` backend and
  `serde_yaml`-compatible API, so parsing behavior is unchanged.
