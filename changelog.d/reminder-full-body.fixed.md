A reminder provider that emits an unchanged non-durable reminder while the earlier copy is still in the
session now sends nothing, restarts the held copy's TTL, and reports the skip with reason
`unchanged_in_context` and the held `reminder_id`. Any change to a declared field, such as authority, TTL
or tags, sends the reminder again. It used to send a
"Reminder `<key>` is unchanged from its previous full emission" pointer with a body hash, which models
repeated back to people. Once compaction or TTL expiry removes the earlier copy, the next emission carries
the full body. Fire reports drop the `delta_compressed` and `original_body_bytes` fields.
