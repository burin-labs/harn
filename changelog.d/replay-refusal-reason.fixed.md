Relocated replay command refusals expose `reason_code:
"replay_relocation_unconfined"` alongside the `tool_rejected` category and
diagnostic message. Agent tool failure receipts preserve caught structured
errors in `error_data`, so hosts can identify the refusal without parsing text.
