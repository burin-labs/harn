- **CI policy workflow decoding retains typed fields (#9347).** Action lifecycle
  fields, runner selectors, timeouts, cancellation controls, and path filters
  are checked at their owning boundary; malformed values are refused instead
  of being silently coerced.
