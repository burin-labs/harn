- **Connector secrets and inline runner ergonomics.** `harn run` now installs
  the configured secret provider chain for package-scoped scripts by default,
  connector OAuth secrets share canonical `namespace/name` parsing and exported
  token-name constants, and `harn run -e` accepts complete Harn programs with
  pipeline entrypoints instead of always wrapping inline source as a snippet.
