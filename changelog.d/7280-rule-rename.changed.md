The `deprecated_llm_options` lint rule is now `removed-llm-options`, and its
repair template `llm/migrate-deprecated-option` is now
`llm/migrate-removed-option` (`HARN-LNT-050` itself is unchanged). Every key
the rule reports was removed outright and is rejected by the runtime, so
"deprecated" — which normally means *still works, warns* — described neither
its severity nor its effect. The new rule id is also kebab-case, matching
every other built-in rule.

`disable_rules = ["deprecated_llm_options"]` keeps working; the old spelling is
still accepted for disabling. Diagnostics report the new id. Tooling that
matches on the repair id needs the new spelling.
