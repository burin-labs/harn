- **Generic type parameters now support multiple `where` bounds.** A
  constraint written as repeated clauses (`where T: A, T: B`) no longer lets
  the second bound clobber the first — both apply, so a method guaranteed by
  the first interface is resolved correctly in the function body. The additive
  spelling `where T: A + B` now parses as well; the two forms are equivalent.
  Method resolution on a multiply-bound `T` accepts a method declared on any
  bound interface, and call-site checking enforces every bound.
