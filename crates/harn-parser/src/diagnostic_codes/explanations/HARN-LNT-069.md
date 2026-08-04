# HARN-LNT-069 — helper can take narrower capabilities

An ordinary function takes the root `Harness`, but every use in its body reads
only one or two capabilities off it. Root authority belongs at entrypoints and
orchestration boundaries. A reusable helper should ask for what it actually
uses, so a reader can tell from the signature what the helper can touch.

## How to fix

When the helper uses one capability, change the parameter to the `Harness*`
type the diagnostic names and pass that sub-handle at each call site:

```harn
fn load_manifest(fs: HarnessFs, path: string) -> string {
  return fs.read_text(path)
}

fn main(harness: Harness) {
  harness.stdio.println(load_manifest(harness.fs, "harn.toml"))
}
```

When it uses two, take them as one record rather than two parameters, so each
call site names what it grants:

```harn
fn refresh_index(io: {fs: HarnessFs, tools: HarnessTools}, path: string) {
  io.tools.invoke("index", {source: io.fs.read_text(path)})
}

fn main(harness: Harness) {
  refresh_index({fs: harness.fs, tools: harness.tools}, "src/index.md")
}
```

A record keeps each grant named at the call site. Two positional handles are
easy to swap by accident, and the swap still type-checks if the shapes are
similar.

`harn fix --apply --safety surface-changing` performs both rewrites. It changes
the parameter, updates every use inside the helper, then narrows the argument at
the call sites it can see (`harness` becomes `harness.fs`, or becomes
`{fs: harness.fs, tools: harness.tools}`). It reuses the existing parameter
name so the new binding cannot shadow anything else in scope, which is why a
narrowed parameter can come out of this repair still called `harness`.
[HARN-LNT-073](HARN-LNT-073.md) reports that and renames it, so running
`harn fix` again finishes the job.

## When to keep root `Harness`

Keep it when the function genuinely coordinates several capabilities or hands
authority onward. Runtime entrypoints keep it too, because the host calls those
signatures directly: `main`, jobs, trigger handlers, registered callbacks, and
the standard connector exports. The lint reads connector exceptions from the
connector ABI registry, and stays quiet whenever authority escapes the function
or the surrounding code cannot prove that narrowing is safe.
