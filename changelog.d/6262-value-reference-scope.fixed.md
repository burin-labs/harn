`harn fix` no longer refuses a capability migration because some unrelated file
binds the callable's name.

The first-class-reference check collected every identifier in the program and
intersected that with the callable set. The set is whole-program on purpose, so
a registry in one module can freeze a handler defined in another, but that
reach also meant an ordinary `const repo_root = ...` anywhere in a corpus froze
every `repo_root` helper in it. Local `let`/`const` bindings, destructuring
patterns, and parameter names are now excluded, so only a real value read
freezes a signature.

On a corpus of 365 capability-migration errors this moved `harn fix
--capability-migrations-only` from 6 applied repairs to 362.
