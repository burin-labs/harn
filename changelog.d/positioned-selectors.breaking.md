Rename selectors now use exact defining paths, indexed qualified aliases and
1-based UTF-16 declaration-name columns across supported languages. Decorated
and exported graph declarations report the name line, and language-server rename
uses that same selected position instead of the first matching name on a line.

Migration: use the defining path and declaration coordinates returned by the
index rather than a suffix path or an outline wrapper line. Pass `path: "."`
for an intentional workspace-wide seed search. Wrong supplied constraints now
return no match. Ambiguous-rename `warnings[*].symbol` now carries the
qualified name in every language, matching `qualified_name`. Move and
signature-change path rules are unchanged.
