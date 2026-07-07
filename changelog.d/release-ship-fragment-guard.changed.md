- **`release_ship.sh` now refuses to ship unfolded changelog fragments.** A new
  `require_no_unfolded_fragments` preflight (run first in every release mode,
  before the audit) fails loud with the exact remediation if
  `changelog.d/<id>.<category>.md` fragments remain, instead of silently cutting
  a release whose notes omit them. Closes the gap where invoking `release_ship`
  directly (bypassing the `release_harn` fold) produced empty release notes.
