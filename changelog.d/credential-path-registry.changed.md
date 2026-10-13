- Credential paths now come from one registry, `known_paths.toml` in harn-vm,
  and every guard reads it: the sandbox read denylist, the taint gate that
  asks before reading a secret under untrusted context, the command risk scan's
  `credential_file_read` label, and the default approval
  `sensitive_path_patterns`. Before, they kept separate lists, so for example
  reading `~/.docker/config.json`, `~/.config/gcloud`, or `~/.kube/config`
  under untrusted context never tripped the secret-read gate, and the approval
  rules missed `~/.ssh` and every cloud CLI store. The sandbox now also denies
  `~/.pgpass` and `~/.git-credentials`. The path matchers now treat `.envrc`,
  `id_ecdsa`, `id_dsa`, `*.key`, `secrets.*`, and `*.secrets.*` as credentials
  everywhere. A substring match no longer fires on lookalike names such as
  `.environment.ts`.
- The PackageManagerConfig preset no longer lists `~/.npmrc`, `~/.netrc`,
  `~/.pypirc`, or Cargo's credential files as grants. The denylist already
  removed them, and the registry now refuses a grant that names a credential.
