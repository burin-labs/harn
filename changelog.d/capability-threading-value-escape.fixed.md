- **Capability threading no longer breaks a callable whose value is used as a
  first-class reference.** A registry entry like
  `handler: web_search_handler` is dispatched as `handler(args)` through a
  stored reference, so the callable's parameter list is observable at a call
  site no static pass can see. Threading a leading capability into it moved
  `args` into the capability slot at runtime, and `harn check` reported nothing
  because the call goes through a value rather than a name. The escape is now
  observed across the whole program — a handler defined in one module and
  registered in another is the common shape — and such callables keep their
  arity, leaving the site for a human to wrap as
  `{ args -> handler(harness, args) }`.
