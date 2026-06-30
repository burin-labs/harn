- Fan-out agent workers now preserve and isolate the active host bridge across
  cooperative awaits, so child `host_call` operations keep using the intended
  host bridge when sibling tasks interleave.
