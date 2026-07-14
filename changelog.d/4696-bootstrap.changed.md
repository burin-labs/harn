- **Harn binary bootstrap now reuses the shared Cargo resolver in release and
  audit helpers (#4696).** Release gates, audit fan-out, and Makefile Harn lint
  targets no longer parse Cargo metadata with embedded Python to find the debug
  `harn` binary.
