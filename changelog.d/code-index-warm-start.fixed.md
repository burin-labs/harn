The code index now starts warm in a new process. `harness.code_index.rebuild` on an
empty index restores that root's valid on-disk snapshot and reconciles it against
disk, reporting `mode: "restored"`, instead of walking and parsing the whole tree
again. A snapshot for another root, another format version, or one that fails to
decode still falls back to a full build. `harn serve acp` starts a background warm
for each session's project root when the session opens, without blocking the first
prompt; set `HARN_DISABLE_CODE_INDEX_WARM=1` to turn it off. `code_index.status`
reports the `root` the live index belongs to, or `nil` when none is loaded.
