# HARN-LNT-074 - unused private pipeline input

## What it means

An underscore-prefixed input on a private, unextended pipeline is unused and
has no default, rest, or caller-owned contract. Unattributed host and operational
pipelines and bare `@test` declarations qualify. Unattributed `test_*` pipelines
and other attributes retain their runner, fixture, and table-owned inputs.

## How to fix

Review external host callers, then apply the surface-changing fix explicitly to
remove the input. Keep and type a named input when the pipeline uses the value.
