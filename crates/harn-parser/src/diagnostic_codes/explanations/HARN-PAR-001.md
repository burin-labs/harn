# HARN-PAR-001 — parser found an unexpected token

**Category:** Parser (PAR)  
**Variant:** `Code::ParserUnexpectedToken` (parser unexpected token)

## What it means

The lexer or parser raises this before type checking even starts. Harn cannot
build an AST from the source until the offending token sequence is repaired.

Specifically: parser found an unexpected token.

## How to fix

- Re-read the source around the highlighted span and restore the missing token(s).
- If the surrounding construct is a multi-line expression, check brace / bracket balance.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
