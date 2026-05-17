# HARN-FMT-003 — formatter normalized trailing comma layout

**Category:** Formatter (FMT)  
**Variant:** `Code::FormatterTrailingComma` (formatter trailing comma)

## What it means

The formatter could not produce a canonical layout for this source — either
because parsing failed first, or because the existing layout drifts from the
canonical form.

Specifically: formatter normalized trailing comma layout.

## How to fix

- Run `harn fmt` to bring the file to canonical form, or fix the parse error blocking the formatter.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
