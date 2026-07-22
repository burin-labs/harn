- `std/bump/live`: `__json_or_nil` now unwraps its `try` result instead of
  leaking the Ok enum into record positions, which made every REST helper —
  including the release-readiness poll that gates live bumps — fail with
  "parameter `a` expects {...R1}, got enum" under the typed runtime.
