- **Multi-line string interpolation.** An indented `"""` string keeps the
  spacing and newlines next to `${...}`: `Hello ${name} and welcome` no longer
  loses the space after the name, and `${a} ${b}` no longer collapses to `ab`.
- **Compile-time folding.** A negative list index counts from the end, and
  `round(x, digits)` shares the runtime's implementation, so `@test` case
  arguments match what the same expression returns at runtime.
- **Debugger.** Hit counts and triggered breakpoints persist across Continue
  and steps, `setBreakpoints` answers only the requested file, and the stack
  trace lists the current frame first, matching `restartFrame`.
