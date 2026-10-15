- Agent-session events in `.harn/session-store.sqlite` now use payload schema 2:
  every payload carries a `record` tag (`transcript`, `compaction`, or
  `message_removed`), and every row carries the
  `harn.agent_session_event.schema: 2` header. The journal writer and every
  reader (run-record projection, timeline, recap, restore, spend) decode one
  typed payload instead of reading JSON pointers. The transcript envelope keeps
  its field names, so schema-1 rows still read unchanged and scripts that read
  `payload.transcript_event` keep working. Rows written by schema 2 hash
  differently in the signed receipt chain because they carry the tag and header.
- A tool call is one typed fact everywhere: run records, `harn runs report`, and
  session bundles all carry the run record's `ToolCallRecord`, whose `status`
  (`open`, `completed`, `failed`, `rejected`) replaces `is_rejected`. The run
  report is now `harn.run_report.v2` (schema version 2) and its tool-call rows
  use `tool_use_id` instead of `call_id`. A version-1 report is refused with a
  schema error. Run records and bundles written with `is_rejected` still read,
  and a recorded rejection becomes `status: "rejected"`.

  Migration: read `status` instead of `is_rejected`, and `tool_use_id` instead
  of `call_id` in report tool-call rows; regenerate stored run reports with
  `harn runs report`.

  ```json
  // harn.run_report.v1 tool_calls[] row
  {"agent_id": "run:root", "call_id": "call-1", "tool_name": "add", "is_rejected": false}
  // harn.run_report.v2 tool_calls[] row
  {"agent_id": "run:root", "tool_use_id": "call-1", "tool_name": "add", "status": "completed"}
  ```

  In Rust, `harn_vm::session_bundle::BundleToolCall` is removed (bundles hold
  `ToolCallRecord`), and `RunReportToolCall` is `{ agent_id, call: ToolCallRecord }`.
