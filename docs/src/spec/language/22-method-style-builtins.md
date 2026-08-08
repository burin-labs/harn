<!-- Generated from spec/chapters/*.md by scripts/sync_language_spec.harn -->

## Method-style builtins

If `obj.method(args)` is called and `obj` is an identifier, the interpreter first checks
for a registered builtin named `"obj.method"`. If found, it is called with just `args`
(not `obj`). This enables namespaced builtins like `experience_bank.save(...)`
and `negative_knowledge.record(...)`.

