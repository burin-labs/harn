# AST parse observations

`std/ast` exposes the installed parser through typed observations. It preserves
the native parser-health receipt instead of guessing grammar reliability from
the language name, error span, bracket balance, or number of declarations.

```harn
import { ast_parse_delta } from "std/ast"

fn main(harness: Harness) {
  const result = ast_parse_delta(harness.ast, {
    before: "fn example() {}\n",
    after: "fn example() { let value = ; }\n",
    language: "rust",
  })
  print(json_stringify(result))
}
```

## Contracts

`ast_parse_errors(ast, source, language)` observes one source string.
`ast_parse_errors_result(value, language)` decodes an already obtained native
observation. Coordinates are zero-based except the one-based display `line`.
Malformed coordinates or contradictory support and health produce an explicit
unavailable observation. Missing provenance never establishes a clean parse.

`ast_parse_delta(ast, request)` compares the source before and after an edit.
Omit `before` for a new file. It parses each distinct source once, relocates
findings in the unchanged suffix, and compares findings individually. Replacing
one old defect with a different defect still produces a finding when the total
error count stays equal.

| Status | Meaning |
| --- | --- |
| `clean` | The health receipt verifies source, including externally resolved parser errors. |
| `unchanged` | Observed errors remain, but this edit added no finding. |
| `inconclusive` | Findings or source coverage remain unverified. |
| `unavailable` | This installed parser cannot provide usable coverage. |

`finding_count` reports every fresh finding. `findings` retains at most twenty;
`primary` selects the smallest error span for located feedback. Before and after
error counts are nullable: `null` means no attested measurement, whereas `0`
means an observed zero. An unchanged error set does not establish clean source.

Parser findings alone cannot establish that source is invalid. An installed
compiler or another named independent authority must resolve inconclusive
coverage. Hosts retain approval, mutation, rollback, and presentation ownership.
