# HARN-OWN-003 — owned value escapes its valid scope

## What it means

A binding annotated with `owned<T>` carries sole ownership of a drop-able
resource (a file, channel, MCP session, transcript writer, sync permit). The
compiler emits an implicit `defer { drop(x) }` at the binding's enclosing
block so the resource closes deterministically when control leaves the scope.

Returning the binding by name — or otherwise transferring it out of the scope
without declaring the transfer in the type — defeats that contract: the auto-
drop never fires, and the resource leaks until the runtime garbage-collects
the handle (which, for OS resources, may be never).

```harn
fn open_log() -> channel {
  const ch: owned<channel> = channel("log", 64)
  return ch    // HARN-OWN-003: `ch` escapes; auto-drop is bypassed
}
```

## How to fix

- **Transfer ownership explicitly** by declaring the function's return type as
  `owned<T>`. The caller then receives an owned binding and is responsible for
  dropping it (or transferring it on again):

  ```harn
  fn open_log() -> owned<channel> {
    const ch: owned<channel> = channel("log", 64)
    return ch    // OK — ownership flows to the caller
  }
  ```

- **Drop the value before returning** by wrapping the work in a block whose
  exit triggers the auto-drop, then returning a non-owned summary:

  ```harn
  fn write_log(msg: string) -> nil {
    {
      const ch: owned<channel> = channel("log", 64)
      send(ch, msg)
    }   // `ch` drops here, before the function returns
    nil
  }
  ```

- **Drop the value explicitly** with `drop(x)` if you need to release earlier
  than the enclosing block would close it.
