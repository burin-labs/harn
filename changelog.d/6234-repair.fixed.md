`HARN-LNT-029`, the lint face of the boundary-validation rule, offered
`types/add-shape-annotation` as its one-step repair. An annotation is erased
before the value exists, so applying that repair silenced the lint and left the
payload unchecked — the escape the rule exists to close, one keystroke away and
reachable without reading the help text that no longer mentions it. The repair
is now `types/validate-boundary-value`, which points at `schema_expect()` and
`schema_check()`. `HARN-LNT-060` keeps the annotation repair: it is about an
inline options dict bypassing the typed option constructors, where no untrusted
payload is involved.
