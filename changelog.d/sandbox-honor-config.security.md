- **The ACP `code` (ActAuto) coding mode now honors an embedder-supplied
  sandbox config instead of discarding it.** Previously, an embedder's
  `AcpSandboxConfig` (e.g. from `sandbox.json` / `BURIN_SANDBOX_CONFIG`) was
  loaded and validated but silently dropped in `code` mode, because the ActAuto
  tier short-circuited to "no per-turn policy" before the config was applied —
  so the default coding agent ran with no filesystem scoping, no OS
  confinement, and no egress guard even when the embedder asked for them.
  ActAuto's *approval* semantics (no human approval gate) are now decoupled
  from *OS confinement*: when the embedder provides a non-default sandbox
  config, `code` mode applies it as a `Worktree`-level OS sandbox (seatbelt on
  macOS, Landlock on Linux) seeded from the config's read-only roots and
  process presets, while keeping ActAuto's `network` side-effect ceiling. **No
  change to the no-config default** — a session with no sandbox config behaves
  exactly as before (ambient policy, no per-turn ceiling).
- **The SSRF private-address egress guard is now installed on the ACP
  agent/serve path** (not just `harn run`) whenever the embedder opts into
  sandboxing. While active it blocks outbound requests to private / loopback /
  link-local / cloud-metadata addresses (e.g. `169.254.169.254`, `127.0.0.1`,
  `10.x`, `192.168.x`) while leaving legitimate public traffic — model API
  calls, `web_search` / `web_fetch` to public hosts — fully allowed. The
  metadata endpoint stays blocked even with the loopback escape hatch. Local
  model servers on loopback are reached via the documented
  `HARN_EGRESS_ALLOW_LOOPBACK=1` / `egress_policy({block_private:"off"})` opt
  out. With no sandbox config the guard is not installed, so default egress is
  unchanged.
- **The `DeveloperToolchains` sandbox preset now covers JVM/iOS toolchain
  caches** so a `Worktree`-confined build does not break Gradle, Maven,
  CocoaPods, Xcode, or Kotlin/Native. Read+write access is granted to
  `~/.gradle`, `~/.m2`, `~/.konan`, `~/Library/Caches/CocoaPods`, and
  `~/Library/Developer/Xcode/DerivedData` when the policy allows workspace
  writes (read-only otherwise), mirroring the existing `UserTemp` cache-write
  pattern.
