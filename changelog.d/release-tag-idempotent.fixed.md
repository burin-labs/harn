`release_ship.sh --finalize` now skips a redundant `git push origin <tag>` when
origin already has the release tag at `HEAD`, avoiding pre-push hook failures
during tag-triggered release finalization.
