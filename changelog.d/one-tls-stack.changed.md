- **HTTPS and WSS connections now trust the operating system's certificate
  store, and the CLI builds one TLS stack.** Every HTTP client moved to
  reqwest 0.13 with rustls on the aws-lc-rs crypto provider, so the default
  CLI no longer compiles the `ring` provider or a second copy of reqwest.
  Server certificates are verified through the platform verifier (Keychain on
  macOS, the Windows certificate store, the system CA bundle on Linux) instead
  of a bundled Mozilla root list, so a corporate proxy or private CA that the
  machine already trusts works for `http_*`, `sse_connect`,
  `websocket_connect`, LLM providers, and package fetches without extra
  configuration. On Linux the system bundle must exist: a minimal container
  image needs the `ca-certificates` package, or `SSL_CERT_FILE` pointing at a
  PEM bundle. Release archives are now built with a named `dist` Cargo
  profile (`cargo build --profile dist`) that owns symbol stripping, so a
  from-source `Dockerfile` build ships the same stripped binary as the
  release archives.
