- Host test and package tools: libtest rows ignored with a reason count as
  skipped, a root-level `test_*.py` selects pytest, Gradle and Maven test
  filters use `--tests` and `-Dtest=`, `uv` update names packages with
  `--upgrade-package`, Go removal uses `@none`, Swift `add` reports that it is
  unsupported instead of resolving, and `dev: true` reaches Cargo, uv,
  Composer and Bundler.
