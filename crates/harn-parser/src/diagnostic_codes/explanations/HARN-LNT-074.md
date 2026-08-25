# HARN-LNT-074 - unused private pipeline input

## What it means

An underscore-prefixed input on a private pipeline is not used by its body, a
local caller, a fixture, or a table-driven test. Unlike a function or callback
parameter, this input does not own a fixed positional slot.

## How to fix

Apply the surface-changing fix to remove the input. Keep and type a named input
when the pipeline uses the value. Public, extended, called, fixture-bound, and
table-bound pipelines preserve their positional contracts instead.
