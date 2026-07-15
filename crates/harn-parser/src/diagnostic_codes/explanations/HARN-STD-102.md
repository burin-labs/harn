# HARN-STD-102 — public stdlib function missing return type

## What it means

Every public function exported from Harn's embedded standard library is a
contract boundary. The function's return type is part of that contract: callers,
generated docs, LSP hovers, schema export, and downstream agents should be able
to depend on the producer shape without rediscovering it from implementation
details.

This lint warns when a `pub fn` declared inside
`crates/harn-stdlib/src/stdlib/**/*.harn` omits an explicit `-> Type`
annotation. Private helpers remain inferable.

## How to fix

Declare the narrowest honest return type in the function signature:

```harn,ignore
pub fn read_json(path: string) -> Result<JsonValue, JsonReadError> {
  ...
}
```

Use named closed records for finite object shapes, `Result<T, E>` for fallible
operations, and typed maps such as `dict<string, V>` for true open-key maps.
Do not silence the lint with `any` or open `dict` unless the value is genuinely
opaque and the caller must narrow it before use.
