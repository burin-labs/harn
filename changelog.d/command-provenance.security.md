- **Command-argument provenance (opt-in).** Under `taint_command_reads`,
  untrusted-origin file provenance extends from structured `read_file` calls to
  the command surface: an `Execute`-kind tool whose command string names a
  tainted-origin file (`cat vendor/dep/README`) is classified untrusted by the
  same file origin, so a payload laundered back into context outside a
  structured read still arms the taint / lethal-trifecta gate. This closes the
  `tool_result` residual — the fetch-to-disk-then-`cat` laundering path that
  evaded lexical file provenance. It fires only on paths already recorded
  untrusted (via taint-on-write), so a first-party `cat src/main.rs` stays
  trusted and no new confirmations land on ordinary command use. Default OFF
  (byte-identical behaviour when disabled). With this on alongside directive
  authentication and file provenance, the containment battery reaches full
  coverage of its worst-case corpus (every modelled ingress — fetch/MCP
  provenance, cross-agent channel, on-disk read, and laundered command read — is
  contained).
