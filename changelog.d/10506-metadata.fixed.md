Directory metadata now detects same-second edits and distinguishes filenames
containing separators or invalid Unicode. Unreadable directories raise errors
instead of receiving an empty-directory fingerprint. Refresh preserves hashes for
removed directories so they stay stale while live directories can refresh; other
read failures preserve all previous hashes. Existing stored fingerprints become
stale once on upgrade. (#10528)
