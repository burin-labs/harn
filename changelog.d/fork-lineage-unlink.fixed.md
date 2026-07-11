- Fixed `agent_sessions::fork` leaving a dangling `child_id` on the parent when
  the post-fork transcript budget check rejected the fork. The lineage edge is
  now unlinked before the destination session is closed.
