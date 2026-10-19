The reusable bump workflow accepts `behavior-preserving-fix-exclude-codes`, a
space-separated list of diagnostic codes its unattended behavior-preserving pass
must not apply. Every other format-only or behavior-preserving repair still runs,
so a caller can keep new migrations such as `HARN-NAM-013` while declining one rule.
