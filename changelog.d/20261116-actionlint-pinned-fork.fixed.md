The local workflow lint now requires the pinned actionlint build that CI uses
and names the version it found otherwise, so an older actionlint no longer
reports false syntax errors on workflows that use background steps. Developer
setup replaces a mismatched actionlint instead of keeping it.
