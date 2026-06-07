- **`harn providers export` / `providers validate --check-artifacts` now
  generate the `spec/provider-catalog/*` artifacts hermetically.** Generation
  reads only the compiled-in embedded provider config and capability matrix,
  ignoring the developer's `~/.config/harn/providers.toml`, environment
  overrides (`HARN_PROVIDERS_CONFIG`, `HARN_DEFAULT_PROVIDER`, `HARN_LLM_*`),
  the process runtime-catalog overlay, and ambient thread-local user overrides.
  Previously, artifact generation merged the effective (home/env-aware) config,
  so a developer's personal aliases/providers could leak into the shipped
  catalog and clean CI would then flag the artifacts as drifted. Runtime catalog
  presentation is unchanged and still reflects the host's live configuration; an
  explicit `--overlay` file remains honored because it is a declared,
  reproducible input rather than ambient machine state.
