`json_decode` restores byte values from their existing base64 JSON envelope when the target schema calls for bytes,
including records, lists, maps and unions. Byte schemas now export valid JSON Schema and OpenAPI carrier objects.
Untyped JSON parsing preserves the envelope as a dictionary.
