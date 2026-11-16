UI resource registration metadata and JSON-RPC message parameters now retain
their generated field types. Host capability normalization decodes the MCP,
legacy app, and OpenAI app dialects once while preserving their precedence,
scalar coercions, nominal records, and malformed-field refusals. Runtime
payload values and extension fields remain unchanged. Refs #9347.

### Migration

Treat application-specific tool arguments, context values, and browser
permissions as opaque until decoded. Replace direct field access such as
`call.params.arguments.window` with a schema check:

```harn,ignore
type RefreshArguments = {window: string}
const arguments = schema_expect(call.params.arguments, schema_of(RefreshArguments))
harness.stdio.println(arguments.window)
```

Use the same pattern for a value from
`update.params.structuredContent.selection` or
`registration.meta.ui.permissions`. Generated fields such as
`call.params.name` and `registration.meta.ui.csp.connectDomains` now reject
incompatible reads and writes during source checking.
