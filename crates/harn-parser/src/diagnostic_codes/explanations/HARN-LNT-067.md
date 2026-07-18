# HARN-LNT-067 — public callable type is missing

A public function or pipeline parameter or return has no explicit type while
the package enables complete public API contracts.

Untyped private callables remain inferable. Public callables cross a module
boundary, so their contract must be visible without inspecting the body or a
particular caller.

```harn
pub fn load(path) {                  // warning: parameter and return are untyped
  return {path: path}
}

pub pipeline deploy(config) {        // warning: parameter and return are untyped
  return true
}
```

## How to fix

Annotate every public parameter and return:

```harn
pub fn load(path: string) -> {path: string} {
  return {path: path}
}

pub pipeline deploy(config: DeployConfig) -> bool {
  return true
}
```

Use `unknown` or `any` when the boundary is intentionally dynamic. Those are
explicit contracts; omitting the annotation is not.

Enable this policy for a package with:

```toml
[lint]
require_public_api_types = true
```

or for one lint invocation with `harn lint --require-public-api-types`.

The rule does not choose or insert a type automatically because selecting a
public contract is an API-design decision.
