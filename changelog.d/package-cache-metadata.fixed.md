Frozen and offline package installs tolerate interrupted writes of Harn's own
cache metadata. Incomplete metadata files remain outside package content hashes
and installed generations; other unexpected files still fail integrity checks.
