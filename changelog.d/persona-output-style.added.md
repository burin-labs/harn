- Added a persona `output_style` field. A persona manifest (or `.harn`
  `@persona(...)`) can now declare how the persona shapes its prose — either a
  bare style name (`output_style = "concise"`) or a `{ name, instructions }`
  table. The new `persona_output_style(function?)` accessor (from
  `std/personas/prelude`) returns the active — or a named — persona's style as
  `{name, instructions}`, or nil when none is declared.
