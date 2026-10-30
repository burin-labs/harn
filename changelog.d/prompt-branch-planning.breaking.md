`run_tape::plan_prompt_branch` chooses an origin edit's first live model call
from recorded prompt-source attribution and reports the first consumer of each
edit. Unused, unattributed, or ambiguously sourced edits refuse automatic
planning. Rendered values retain their attribution when consumed later.

Migration:

Rust and JSON hosts constructing `PromptSourceEdit` must choose `activation`:
`Origin` (`origin`) edits source when it is rendered, including before an
unaffected recorded request; `AfterPrefix` (`after_prefix`) applies a labeled
mid-run intervention after the replayed prefix. Automatic planning accepts
origin edits. Prefix request verification remains mandatory for both modes.
