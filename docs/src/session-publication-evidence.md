# Session publication evidence

`harn session publication-evidence SESSION --project-root PATH` reads the
existing canonical session store and emits one JSON object with schema
`harn.session_publication_evidence.v1`. This diagnostic command does not start
an agent, create a missing store, or emit transcript prose. Missing sessions,
incomplete event coverage, and a store changing during the read return an error.

## Coverage and origin

`stored_event_count`, `last_event_id`, and `chain_root_hash` describe the complete
stable event read. `assistant_message_count` counts every assistant row, and
`records` retains each row and its `source_event_id`. `missing_source_count`
includes all origins.

Each record's `origin` is one of:

| Origin | Meaning |
| --- | --- |
| `harness_bookkeeping` | The transcript owner's boolean `harn_bookkeeping_turn: true` marks a harness-authored message, such as a withdrawn-answer placeholder. |
| `unmarked_assistant` | The assistant row has no true bookkeeping flag. This does not prove that a model was called. |

`bookkeeping_count` counts the explicitly marked harness rows. Bookkeeping rows
have no model call role, call stage, or publication disposition. Their absent
model metadata does not contribute to missing model metadata counts.

## Model metadata

For unmarked assistants, `call_role`, `call_stage`, stage-metadata presence and
validity, and publication disposition come from the existing transcript owners.
Missing or unrecognized metadata remains counted explicitly. A string or other
malformed bookkeeping flag does not exempt a row from those diagnostics.

`pending_count`, `published_count`, and `withheld_count` count only measured
publication decisions on unmarked assistants. Missing metadata never implies
publication, withholding, or an empty successful model-dispatch census.
