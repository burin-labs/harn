Java call lookup and method rename now use explicit receiver types to keep
unrelated methods with the same name separate. Rename accepts qualified names
and exact declaration positions, and refuses unsupported receiver bindings
before writing.
