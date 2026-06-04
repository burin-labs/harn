- **Vertex honors the modern `output_format` for structured output.** It
  previously read only the legacy `response_format`/`json_schema` mirror, so a
  call using `output_format: {kind: "json_schema", schema}` silently produced no
  structured-output directive on the Vertex backend.
