# GraphQL stdlib

`import "std/graphql"` provides a small provider-neutral substrate for GraphQL-backed
connector packages.

Use it when a connector needs to own GraphQL documents in Harn instead of
hand-assembling request JSON, error envelopes, cursor metadata, and generated
wrapper source in each package.

## Core helpers

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

## Connector example

```harn
import {
  graphql_execute_operation,
  graphql_operation,
  graphql_page_info,
} from "std/graphql"

const issues = graphql_operation(
  "ListIssues",
  "query ListIssues($first: Int, $after: String) {"
    + " issues(first: $first, after: $after) { nodes { id identifier"
    + " title } pageInfo { hasNextPage endCursor } } }",
  {root_field: "issues"},
)

pipeline default(harness: Harness) {
  const envelope = graphql_execute_operation(
    {
      endpoint: "https://api.linear.app/graphql",
      auth: {access_token: harness.secrets.read("linear/token")},
    },
    issues,
    {first: 25},
  )
  const page = graphql_page_info(envelope.result)
  harness.stdio.log(page.end_cursor)
}
```

## Typed responses

Declare the response type next to the query and decode `data` once. Only the
fields the script reads need to appear, and unknown fields pass. Mark a field
GraphQL may null as `T?`; it is still present in the response.

```harn
type IssueNode = {id: string, identifier: string, title: string}

type IssuesData = {
  issues: {
    nodes: list<IssueNode>,
    pageInfo: {hasNextPage: bool, endCursor: string?},
  },
}

fn issue_titles(data: unknown) -> Result<list<string>, string> {
  const decoded = schema_parse(data, schema_of(IssuesData))
  if is_err(decoded) {
    return Err(unwrap_err(decoded).message)
  }
  return Ok(unwrap(decoded).issues.nodes.map({ issue -> issue.title }))
}
```

An inline fragment over a union (`... on CheckRun`, `... on StatusContext`)
decodes as a union of records that each carry a literal `__typename`, and a
`__typename` comparison narrows it. See "Decode at the boundary" in the
error-handling guide.

`auth` accepts `{access_token}`, `{api_key}`, `{token, scheme}`, or
`{authorization}`. Rate-limit metadata is collected from common `X-RateLimit-*`,
Linear endpoint, and complexity headers.
