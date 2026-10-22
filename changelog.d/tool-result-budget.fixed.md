Agent tool results now pass a per-result size budget (`tool_result_max_chars`, default 64000, `0` disables)
before they enter the conversation. A larger result is saved in full to an owner-only file in the workspace
temp dir (`.harn-tmp/tool-results/`), which the agent can read back and which is removed when the loop ends.
The model sees a head/tail/diagnostic excerpt with a notice naming that file. The result records
`tool_output_truncated`, `dropped_bytes`, `original_size`, `final_size`, and `full_output_path`. The compaction
`tool_output_max_chars` clamp now also rewrites `tool_result` messages and framed text-channel results, which it
previously skipped.
