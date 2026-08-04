# HARN-LNT-072 — builtin exists, but Harn source cannot name it

Every builtin declares an *exposure* saying which surface may reach it. Two of
those values are closed to scripts:

- `privileged_wire` — a trusted embedder primitive. Only artifacts stamped with
  privileged provenance may call it; user modules cannot name or re-export it.
- `runtime_internal` — a compiler or runtime implementation detail that is never
  source-visible.

The VM still registers those builtins, because the runtime and the host bridge
call them. That is why this reads as a lint rather than an unknown name: the
name exists, it is simply not yours to call, and the typechecker will reject the
program.

A `Harness` capability method also reports here when it is called as a bare
global — the method is real, but it is reached through its handle.

## How to fix

Call the surface the diagnostic names.

For a capability method, thread the handle:

```harn
fn summarize(obs: HarnessObs, message: string) {
  obs.log_info(message)
}
```

For `host_call`, there is no single replacement, because it was a generic
dispatcher rather than one operation. Almost every target it dispatched to has a
declared capability method, so the rewrite is per-namespace:

```harn
host_call("ast.outline", {path: p})   // before
harness.ast.outline(p)                // after
```

Check `harn contracts builtins` for the declared method — the namespace in the
`host_call` string is usually the capability name, and `pr_monitor`-style
underscores are the only common spelling difference. An operation with no
declared contract goes through the callable root your host installs with
`register_callable_host_operation`, reached as `<root>.<operation>`.

Argument shapes do not carry over. `host_call` took a params dict, while a typed
method takes the parameters its signature declares.

If a script genuinely needs a privileged wire, it has to run as a privileged
artifact; calling it from ordinary source will not work regardless of how the
call is spelled.
