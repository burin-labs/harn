# HARN-PAR-002 — parser reached end of file while expecting syntax

## How to fix

- Re-read the source around the highlighted span and restore the missing token(s).
- If the surrounding construct is a multi-line expression, check brace / bracket balance.
