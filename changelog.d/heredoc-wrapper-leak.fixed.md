Stop a leaked tool-call heredoc wrapper from corrupting written files. When a model delivers a
`content: <<EOF\n...\nEOF` value through a channel that never runs the heredoc grammar — a native
JSON string `"<<EOF\n...\nEOF"`, or chat-template `<parameter=content>`/DSML markup — the `<<TAG`
opener and closing sentinel previously leaked verbatim into the file, so the first line became a
literal `<<EOF` (e.g. Zig: `expected type expression, found '<<'`) and the build failed. The
dispatch normalizer (`normalize_tool_args`) now strips a value that is *entirely* one well-formed
heredoc, recursing into nested `ops` arrays. A value that merely contains `<<` (a shift operator, a
real mid-file `<<EOF`) or a partially-wrapping heredoc is left byte-identical.
