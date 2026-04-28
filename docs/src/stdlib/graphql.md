# GraphQL Stdlib

`import "std/graphql"` provides a small provider-neutral substrate for GraphQL-backed
connector packages.

Use it when a connector needs to own GraphQL documents in Harn instead of
hand-assembling request JSON, error envelopes, cursor metadata, and generated
wrapper source in each package.

## Core Helpers

- `graphql_request(endpoint, query, variables?, options?)` sends a GraphQL-over-HTTP
  `POST` request and returns a normalized envelope.
- `graphql_normalize_response(response, options?)` converts HTTP or GraphQL-like
  values into `{ ok, partial, data, errors, extensions, meta }`.
- `graphql_operation(name, document, options?)` captures an operation document plus
  root-field, schema, and persisted-query metadata.
- `graphql_execute_operation(client, operation, variables?, options?)` validates
  variables when `variables_schema` is present, runs the operation, and returns
  the envelope plus `result`.
- `graphql_generate_client(operations, options?)` emits Harn source for generated-style operation wrappers.
- `graphql_parse_schema(sdl)` parses lightweight SDL fixtures into type records.
- `graphql_introspection_query()` and `graphql_schema_from_introspection(payload)` normalize introspection responses.

## Connector Example

```harn
import {
  graphql_execute_operation,
  graphql_operation,
  graphql_page_info,
} from "std/graphql"

let issues = graphql_operation(
  "ListIssues",
  "query ListIssues($first: Int, $after: String) { issues(first: $first, after: $after) { nodes { id identifier title } pageInfo { hasNextPage endCursor } } }",
  {root_field: "issues"},
)

pipeline default() {
  let envelope = graphql_execute_operation(
    {endpoint: "https://api.linear.app/graphql", auth: {access_token: secret_get("linear/token")}},
    issues,
    {first: 25},
  )
  let page = graphql_page_info(envelope.result)
  println(page.end_cursor)
}
```

`auth` accepts `{access_token}`, `{api_key}`, `{token, scheme}`, or
`{authorization}`. Rate-limit metadata is collected from common `X-RateLimit-*`,
Linear endpoint, and complexity headers.
