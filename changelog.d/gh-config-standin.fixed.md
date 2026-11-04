Confined commands now get a credential-free `GH_CONFIG_DIR` stand-in. GitHub CLI exits on the denied
`hosts.yml` even when `GH_TOKEN` is set, so `gh` failed with a permission error inside every sandbox.
The stand-in carries `config.yml` (aliases, git protocol, editor) and never `hosts.yml`.
