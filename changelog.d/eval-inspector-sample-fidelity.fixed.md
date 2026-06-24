The `harn.eval.inspect_run` dossier now reports sample-scoped event stats
accurately. `first_sampled_id`/`last_sampled_id` and the provenance
`chain_breaks_in_sample`/`sample_chain_ok` cover only the sampled prefix window
instead of leaking full-scan values when a JSONL topic has more records than
`limit`, and `agent_event_topics` is deduplicated when a topic surfaces from
both the JSONL dir and the sqlite log.
