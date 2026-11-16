Preserve open maps and ordinary JSON Schema constraints in non-strict Anthropic
tool definitions. Strict tools and structured outputs retain strict projection.

Migration: Rust consumers matching `SchemaCompatProfile` must handle
`AnthropicLenient`. Schemas with arbitrary keys no longer become empty closed
objects unless the tool explicitly requests `strict: true`.
