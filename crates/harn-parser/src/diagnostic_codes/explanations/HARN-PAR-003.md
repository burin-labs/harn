# HARN-PAR-003 — lexer found an unexpected character

## How to fix

- Re-read the source around the highlighted span and restore the missing token(s).
- If the surrounding construct is a multi-line expression, check brace / bracket balance.
