- **Release publishing now fails closed without a pre-pushed tag.** The
  push-to-main release workflow refuses to tag main HEAD when Cargo.toml is
  ahead of the latest release tag, and `release_ship.sh --prepare` now routes
  through the tag-first release harness.
