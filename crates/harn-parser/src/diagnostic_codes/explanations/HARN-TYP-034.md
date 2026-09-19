# HARN-TYP-034: Predicate variant fields require outcome narrowing

A predicate outcome includes uncertainty and failure variants. Read a field
only after narrowing to variants that all contain it. Match `outcome.kind`
before reading `outcome.value.verdict` in the `verdict` arm. The common `kind`
and `receipt` fields are available without narrowing.

This applies to named property access, indexed access, and destructuring.
Dynamic field names cannot establish that the selected variants contain a field.
