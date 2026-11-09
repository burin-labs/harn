Add the pure `glob_match(pattern, candidate)` builtin, the same matcher capability
policy applies to tool argument patterns, so scripts can report which paths a
scoped write grant admits. A candidate equal to its pattern now always matches,
so a scoped write to a literal path such as `app/[slug]/page.tsx` is admitted.
