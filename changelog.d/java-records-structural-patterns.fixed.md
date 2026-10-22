- Java `record` and `@interface` declarations, and C# `record` declarations and constructors, now appear in AST
  symbols and outlines. Record header components are listed as fields, and methods and compact constructors inside a
  record name the record as their container, so symbol-addressed edits can target them.
- `harn-rules` patterns now match with exact shape: a match is rejected when the target has a child, keyword, or
  comment the pattern neither spells nor binds to a metavariable. Previously `getById($A)` also matched
  `folderService.getById(x)`, `foo($A)` matched `foo(a, b)`, and `let $N = $V;` matched `let mut x = 1;`, and the
  `fix` silently deleted the receiver, the extra argument, or the modifier. Write a metavariable where the target may
  vary, such as `$OBJ.getById($A)`. The `no-console-log` seed rule now matches the callee so it still flags calls
  with any number of arguments.
- Java, C#, C, C++, and PHP expression patterns no longer need a trailing `;` to compile. A pattern written with `;`
  is rooted at the statement, so a fix ending in `;` replaces the target's `;` instead of producing `;;`. PHP patterns
  are parsed as PHP code instead of inline HTML.
