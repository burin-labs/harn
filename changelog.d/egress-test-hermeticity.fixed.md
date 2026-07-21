- **Egress tests are hermetic against ambient `HARN_EGRESS_*` configuration.**
  Under `cfg(test)` the egress policy engine now reads `HARN_EGRESS_*` through
  a thread-local override seam instead of the process environment, so a
  developer's or sandbox's exported egress settings can no longer seed a
  policy a test never asked for (previously the whole `egress::`/`http::`
  SSRF-guard test family failed under an ambient
  `HARN_EGRESS_BLOCK_PRIVATE=off`). All `cfg(test)` egress state is now
  thread-keyed, so the old cross-test env mutex is gone and the tests are
  parallel-safe under plain `cargo test` as well as nextest. The testbench's
  `NetworkConfig::DenyByDefault` now installs a typed egress policy directly
  instead of mutating process-global `HARN_EGRESS_*` variables — which also
  fixes a latent mix where its env-var install overrode only three of the
  five variables and silently combined with ambient `HARN_EGRESS_BLOCK_PRIVATE`
  / `HARN_EGRESS_ALLOW_LOOPBACK` settings.
