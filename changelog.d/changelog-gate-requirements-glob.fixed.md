- **Changelog-fragment gate no longer treats nested source files as pip
  manifests.** The dependency-metadata allowlist matched `requirements.*\.txt` /
  `constraints.*\.txt`, where `.*` crossed `/`, so a non-dependency path such as
  `crates/.../requirements_helpers/seed.txt` matched and could slip a
  source-only change past the changelog gate without a fragment. Tightened both
  to a single path segment (`requirements[^/]*\.txt`, `constraints[^/]*\.txt`);
  genuine `requirements*.txt` / `constraints*.txt` manifests are unaffected.
