`harn fmt` no longer drops or moves comments around doc comments. A doc comment
written after code on the same line (`let x = 1 /// the x`) stays on that line as
a plain `// the x` instead of being deleted or hoisted onto the next declaration.
A plain comment sharing a line with a doc comment is kept above the doc block, and
a doc comment written before code on its own line (`/** doc */ fn f() {}`) now
documents that item instead of being deleted.
