- Fixed a latent release-tooling bug that blocked v0.10.30 asset publication:
  `scripts/publish.harn` declared `--dry-run` as a value-taking `flag` instead of
  a `switch`, so the tag-triggered publish's bare `publish.harn -- --dry-run`
  failed argparse with "flag requires a value" (exit 2) before any crate could
  ship. It is now a boolean `switch`, matching the documented `std/cli/argparse`
  contract and how every caller invokes it.
