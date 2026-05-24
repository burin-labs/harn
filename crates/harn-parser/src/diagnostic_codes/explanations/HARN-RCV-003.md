# HARN-RCV-003 — rescue construct is invalid

## What it means

A recovery construct (`try`, `rescue`) is in an invalid position or is shaped in
a way Harn's structured-error handling does not support.

## How to fix

- Move the construct inside a function body, or wrap it in one.
- Use `try` / `rescue` only around expressions that can produce structured errors.
