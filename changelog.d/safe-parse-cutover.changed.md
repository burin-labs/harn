Removed the ambiguous `std/json.safe_parse` helper in favor of bare
`try { json_parse(...) }` result handling with the documented
`JsonParseFailure` contract.
