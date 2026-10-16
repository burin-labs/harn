# HARN-LNT-080 - `?.` chain over an untyped value

## What it means

A chain of two or more `?.` links reads fields from a value whose type the
checker does not know: `any`, `unknown`, an open `dict`, or a value with no
inferred type. Parsed JSON, command output decoded by hand, and responses
passed around as `dict` all look like this:

```harn,ignore
const pr = response.data?.repository?.pullRequest
const head = to_string(pr?.headRefOid ?? "")
```

Every link hedges against a shape nobody declared. A misspelled field reads as
`nil` and falls through to the default, so the code runs and reports nothing.

The rule fires once per chain, on its second optional link. A chain over a
typed record whose fields are declared optional is real nil handling and is
not reported. The finding is advisory (`info`): existing code still carries
thousands of these chains, so it does not fail `harn lint --strict`.

## How to fix

Declare the shape you rely on and decode the value once, where it enters. When
the value is your own data passed around as `dict`, annotate the parameter or
return type that erased it instead.

```harn
type PullRequest = {headRefOid: string, isDraft: bool, mergedAt: string?}
type Snapshot = {repository: {pullRequest: PullRequest?}}

fn head_of(raw: unknown) -> Result<string, string> {
  match schema_parse(raw, schema_of(Snapshot)) {
    Result.Err(error) -> {
      return Err(error.message)
    }
    Result.Ok(snapshot) -> {
      const pr = snapshot.repository.pullRequest
      if pr == nil {
        return Err("pull request not found")
      }
      return Ok(pr.headRefOid)
    }
  }
}
```

`schema_parse` returns `Result<T, SchemaError>`, so a function that returns a
`Result` can write `schema_parse(raw, schema_of(Snapshot))?` instead. Unknown
fields pass validation. A field written `name: T?` must be present but may be
`null`; `name?: T` may also be absent.

See "Decode at the boundary" in the error-handling guide.
