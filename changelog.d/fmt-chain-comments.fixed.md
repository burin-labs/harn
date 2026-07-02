- `harn fmt` no longer orphans `//` comments that sit between the segments of
  a multi-line method chain (they were relocated to the end of the program at
  column 0). Chain-segment comments now stay anchored above the segment they
  precede, for both `.method(...)` and `?.method(...)` chains.
