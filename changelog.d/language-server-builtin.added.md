`harness.code_index.language_server({path})` reports whether a rename of a
file would reach a language server, which server, and its executable, from the
lookup `rename_symbol` uses, without starting a process. A caller deciding
whether to offer a rename of a name with competing declarations no longer needs
its own copy of the search order.
