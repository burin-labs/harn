Language-server renames through typescript-language-server no longer give
different answers on identical runs. Harn starts it without the syntax-only
server, which answered definition lookups without types while a project was
reloading, so a mention such as `map.get` sometimes resolved to nothing and
the rename refused.
