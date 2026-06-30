- **Host-side edits now feed `files_written`.** Added the
  `agent_session_record_changed_path(path, session_id?)` builtin so a product
  host whose edit/write funnel goes through workspace capabilities (rather than
  the hostlib write chokepoint `auto_capture_for_write`) can report the path it
  mutated into the active agent session's changed-path set. Without this, a
  sub-agent's edits performed via the host write path never reached the set the
  receipt drains, so its `files_written` came back empty — surfaced downstream
  as "wrote 0 file(s)" / "0/N units completed" for a child that really did edit.
