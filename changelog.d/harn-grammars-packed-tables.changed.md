The release binary stores its tree-sitter grammars' parse tables compressed and unpacks each
grammar's tables on its first use, which takes 5 ms or less per grammar. The x86_64 Linux binary
is about 37 MiB smaller.
