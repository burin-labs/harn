When a direct seed download fails, the bootstrap adapter can use the GitHub release-assets API if `jq` is available.
It still verifies the pinned release checksum before running the seed.
