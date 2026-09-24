# HARN-TYP-033: Predicate site identity must be literal and unique

The evaluator's first two arguments are nonempty string literals: a stable site
ID and the question asked of the model. A module cannot declare two sites with
the same ID. Repeated execution of one site is allowed.

Place a repeated evaluation in a typed helper with a literal ID and question.
Pass only the changing input and policy into the helper. Different questions
or source sites need different IDs.
