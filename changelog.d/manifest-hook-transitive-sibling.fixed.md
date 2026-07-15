Manifest-registered lifecycle and tool hook handlers now resolve `pub fn`s
from transitively imported modules on every fire, not only the first. Lazy hook
resolution retained just the handler's entry module, so a handler that called an
imported function which in turn called a `pub fn` sibling in its own module threw
`Undefined builtin: <name>` on the second and later fires — a fresh child VM hit
the lazy callable cache without re-importing the module graph, leaving the
transitively imported module's function registry unreachable. The lazy callable
cache now pins the complete module graph loaded during first resolution, so every
transitively reachable registry and module state stays live for the cache's
lifetime.
