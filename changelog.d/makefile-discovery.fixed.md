Project scanning recognizes literal multi-target Make rules and default Makefile
precedence without advertising variable assignments as test commands. Shared
`makefile_parse` and `makefile_filenames` builtins expose the same source facts.
