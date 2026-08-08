## Known limitations and future work

The following are known limitations in the current implementation that may
be addressed in future versions.

### Type system

- **Definition-site generic checking**: Inside a generic function body,
  type parameters are treated as compatible with any type. The checker
  does not yet restrict method calls on `T` to only those declared in
  the `where` clause interface.
- **No runtime interface enforcement**: Interface satisfaction is checked
  at compile-time only. Passing an untyped value to an interface-typed
  parameter is not caught at runtime.

### Runtime

### Syntax limitations

- **No `impl Interface for Type` syntax**: Interface satisfaction is
  always implicit. There is no way to explicitly declare that a type
  implements an interface.
