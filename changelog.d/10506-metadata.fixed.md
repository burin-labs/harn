Directory metadata now detects same-second edits and distinguishes filenames
containing separators or invalid Unicode. Unreadable directories raise errors
instead of receiving an empty-directory fingerprint; read failures during refresh
preserve all previous hashes. Existing stored fingerprints become stale once on upgrade.
