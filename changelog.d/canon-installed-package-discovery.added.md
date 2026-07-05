- `std/agent/canon` now discovers installed single-pack and manifest-backed
  `harn.canon` package contributions under `.harn/packages` when no explicit
  canon root is configured.
- `harn package check` now accepts contribution-only packages as publishable
  package surfaces instead of requiring a module export or rule pack.
