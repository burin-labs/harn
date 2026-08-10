- **Registered API-key connector setup.** `harn connect <provider>` now follows
  manifest-declared manual API-key authentication, supports secret-safe
  `--from-env` and `--value-file` input, and gives exact recovery commands when
  a connector requires more than one independent secret.
