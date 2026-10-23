- Reject CSV imports with duplicate headers or extra cells instead of losing data.
- Parse XML with the existing `roxmltree` library. Preserve Unicode names,
  namespace prefixes, and spaces around comments and child elements. Reject
  trailing content, duplicate attributes, forbidden characters, and DTDs.
  Bound XML rendering depth as well as parsing depth. The writer sanitizes
  colons in tags and rejects text XML cannot represent.
- Accept JSON byte streams split within UTF-8 characters, and allow streamed
  numbers to continue across chunks. Apply the same number rule to LLM output
  validation, and reject incomplete UTF-8 when a stream ends.
- Keep fractional seconds when adding durations to dates, and reject results
  outside the supported date range.

Migration: give CSV columns unique headers and match each row to those columns,
or parse without headers to keep ragged rows. Remove XML DTDs and declare any
namespace prefixes before parsing. Malformed XML now raises an error.
Use bytes, which XML stores as base64, for data with forbidden text characters.
