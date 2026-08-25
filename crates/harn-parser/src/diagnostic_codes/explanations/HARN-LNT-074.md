# HARN-LNT-074 - unused test pipeline input

## What it means

An underscore-prefixed input on a private pipeline structurally marked with a
bare `@test` is not used by its body or a local caller and is not bound by a
fixture or table-driven case. The Harn test runner derives its invocation from
that declaration, but another host can still select any named pipeline.

## How to fix

Review external host callers, then apply the surface-changing fix explicitly to
remove the input. Keep and type a named input when the test uses the value.
Unattributed, extended, called, fixture-bound, and table-bound pipelines preserve
their positional contracts.
