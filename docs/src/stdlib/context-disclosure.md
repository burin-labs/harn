# Context truncation

`std/context/disclosure` cuts Markdown to an estimated token budget and
renders a note that names how to retrieve the omitted text.

```harn
import {
  disclosure_trailer, truncate_at_section_boundary,
} from "std/context/disclosure"

fn main(harness: Harness) {
  const text = harness.fs.read_text("AGENTS.md")
  const cut = truncate_at_section_boundary(text, 8)
  const note = disclosure_trailer(
    cut.shown, cut.total, "lines", "read AGENTS.md",
  )
  harness.stdio.println(cut.rendered + "\n" + note)
}
```

The caller supplies a recovery action that its reader can perform. These
helpers do not store the full text or check that the action can retrieve it.

## Functions

| Function | Result |
|---|---|
| `disclosure_token_estimate(text: string?) -> int` | Character count divided by four, rounded up. `nil` costs zero |
| `truncate_at_section_boundary(text: string, budget_tokens: int) -> DisclosureTruncation` | Leading lines that fit the estimate, plus the line counts below |
| `heading_boundary_keep_count(lines: list, line_keep_count: int) -> int` | Number of leading lines to retain, clamped to the available lines |
| `disclosure_trailer(shown: int, total: int, unit: string, recovery: string) -> string` | An omission note, or `""` when `shown >= total` |

`DisclosureTruncation` is a closed record:

```harn
type DisclosureTruncation = {
  rendered: string,
  shown: int,
  total: int,
  truncated: bool,
}
```

`shown` and `total` count lines, including empty lines. Text that fits is
returned unchanged. A nonpositive budget keeps no lines. The estimate is
not a provider tokenizer, and the trailer's cost is outside this budget.

## Section boundaries

A boundary is a blank line or a line starting with `#`, after trimming
whitespace. Boundaries inside triple-backtick fences are ignored. The cut
backs up to the last boundary only when doing so retains at least half the
requested lines. Otherwise it keeps the requested cut, which can end inside
a list or code block. A heading at the boundary is omitted with its section;
blank lines between that heading and its body do not advance the boundary.

## Omission notes

For `shown = 2`, `total = 5`, `unit = "lines"`, and
`recovery = "read AGENTS.md"`, the trailer is:

```text
<truncated: showing 2 of 5 lines; full text: read AGENTS.md>
```

A blank `unit` becomes `items`. A blank recovery action throws, even when
nothing was omitted. Pass the action alone; the helper adds `full text:`.

See [Context assembly](../modules.md#stdcontext) for building and budgeting
complete context objects.
