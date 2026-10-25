A language-server `rename_symbol` no longer refuses over leftover mentions the declaration cannot reach.
For a TypeScript or JavaScript binding local to a function, or never exported from its ES module,
mentions in other files and member names such as `args.length` no longer need the server's answer.
For a Rust inherent method, a macro-body path through an unconditional module (`$crate::m::name`)
names the module's item. The 200-mention limit on the post-edit check is gone; the check's time
budget bounds it instead.
