A repeated reminder provider emission now carries its full body every time. The model no longer sees a
"Reminder `<key>` is unchanged from its previous full emission" pointer with a body hash, which it could
repeat back to the user. Provider fire reports drop the `delta_compressed` and `original_body_bytes`
fields; `body_bytes` is always the full body size.
