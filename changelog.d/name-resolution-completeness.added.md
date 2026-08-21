`ast.undefined_names` now reports how far its reading can be trusted, on a new
`resolution` field. A caller can tell a name that is genuinely undefined from
one this single-file analysis simply could not see, instead of treating every
finding as equally certain.

Completeness needs two things to hold. The language sets a ceiling
(`single_file_complete` for Python, JavaScript and TypeScript, where every name
must be imported or bound in the same file; `package_scoped` for Go, where a
sibling file contributes names with no import; `runtime_resolved` for Ruby,
where names are routinely created at runtime). The file can then defeat an
otherwise complete ceiling with a wildcard import, an `eval`, a `setattr`, a
`method_missing`, or a syntax error, and each such construct is named in
`defeaters`. An `analysed` flag distinguishes a file that was checked and came
back clean from one that was never checked at all.
