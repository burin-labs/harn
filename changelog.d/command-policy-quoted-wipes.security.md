- **Command-risk scanner quoted workspace wipes.** Recursive workspace-wipe
  detection now treats shell quotes and `$PWD`/`$(pwd)` workspace-root targets
  the way the shell does, so quoted forms such as `rm -rf "."`,
  `rm -rf "$PWD"/*`, `find "." -delete`, and quoted `sh -c`/PowerShell/cmd
  payloads are denied without blocking scoped cleanup like `rm -rf "build/"`.
  PowerShell `-EncodedCommand` payloads are decoded as UTF-16LE before the same
  destructive-command policy is applied.
