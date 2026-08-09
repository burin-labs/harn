# HARN-FMT-002 — source is not in canonical format

## What it means

The formatter could not produce a canonical layout for this source — either
because parsing failed first, or because the existing layout drifts from the
canonical form.

## How to fix

- Run `harn fmt` to bring the file to canonical form, or fix the parse error blocking the formatter.
