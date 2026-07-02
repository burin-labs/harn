- `round(x, digits)` rounds to a number of decimal places (half away from
  zero, matching the 1-arg form): `round(2.567, 2)` is `2.57`. Negative
  digits round to power-of-ten buckets (`round(1250, -2)` is `1300`), ints
  stay ints when they fit, decimals keep the decimal type, and the 2-arg form
  const-folds in `const` initializers.
