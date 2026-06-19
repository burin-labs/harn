- **`json_stringify` now preserves the decimal point on whole-number floats.**
  A `float` like `2.0` serialized to `"2"` (so `json_parse` read it back as an
  `int`) and disagreed with `json_stringify_pretty`, which emitted `"2.0"`.
  Compact output now routes finite floats through the same serde `Number`
  formatter as the pretty printer, so floats round-trip as floats.
