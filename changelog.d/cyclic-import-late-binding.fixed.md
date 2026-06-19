- **Imports across a module cycle now bind reliably.** A plain `import "m"`
  or `import { name } from "m"` that resolved to a module still mid-load (an
  import cycle) used to silently skip binding the name, so calling it later
  failed with `Undefined builtin: <name>` — and which module got starved
  depended on load order, making the failure look nondeterministic. Cyclic
  imports are now bound late, once every module in the cycle finishes loading,
  for both bare references and calls. A `pub import` re-export across a cycle
  remains unsupported but now fails with a message that names the cycle
  instead of the misleading "imported module was not loaded".
