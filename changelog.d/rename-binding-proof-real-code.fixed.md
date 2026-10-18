`rename_symbol` applies again in ordinary Rust, TypeScript and Python code. Itemless `std` macros, derives and
built-in attributes no longer defeat the binding proof, and type positions such as `T[]`, `[T; N]` and annotations are
accepted. Names shadowed by a generic parameter, arguments of unknown macros, and statement-position macros that could
declare items still refuse.
