# Check documentation examples

Use the documentation checker to verify fenced Harn examples in Markdown with
the same parser, type checker, linter, and formatter as your code.

## Run the repository check

From your prepared Harn checkout, run:

```bash
make check-docs-snippets
```

The summary reports checked and intentionally skipped snippets, failed checks,
and how many snippets reached the formatter and linter. A run that finds no
documentation files or validates no snippets fails.

## Choose a policy for your documentation

Edit [`.harn-docs.toml`](../../../.harn-docs.toml). Its `include` patterns select
files relative to the working directory. The `defaults` table sets the policy;
matching rules override the fields they name, in file order. Overlapping patterns
check each file once. A rule that matches no selected files fails.

For example, keep partial teaching examples at parse-only validation while
requiring complete, formatted introductory examples:

```toml
version = 1
include = ["README.md", "docs/**/*.md"]

[defaults]
validation = "parse"
format = false
lint = "off"
line_width = 74

[[rules]]
include = ["README.md", "docs/getting-started/*.md"]
validation = "check"
format = true
lint = "strict"
```

Use `validation = "check"` for examples that declare their inputs and imports.
Keep `validation = "parse"` for fragments whose surrounding context appears in
the prose. `lint = "advisory"` reports style warnings; `lint = "strict"` makes
warnings fail the check. `format = true` requires canonical formatter output at
`line_width`. The width limit also applies to intentionally skipped examples
because they must fit the rendered page.

Configuration fields are validated before scanning. Unknown keys, unsupported
values, empty patterns, and nonpositive widths fail with an error.

To use another policy file, pass its path to the checker:

```bash
HARN_BIN=/path/to/harn harn run --standalone \
  scripts/check_docs_snippets.harn -- --config docs-policy.toml
```

## Mark an example's intent

An ordinary `harn` fence uses the file's validation policy. A `harn,check` fence
always receives full type checking. A `harn-prompt` fence uses the prompt-template
linter. An unknown Harn fence modifier fails instead of silently escaping checks.

Use `harn,ignore` or `harn-prompt,ignore` only for intentional fragments that the
parser cannot accept. To show Markdown syntax itself, put it inside a longer
Markdown fence:

````markdown
```harn,check
pub fn answer() -> int {
  return 42
}
```
````

For examples that intentionally demonstrate an error, use
`harn,diagnostic-check` or `harn,diagnostic-lint`. Those fences verify the expected
diagnostic rather than requiring clean code, and do not receive formatting or
ordinary lint checks. Harn's own configuration also sets `diagnostic_examples`
to store the checked diagnostic projection. After changing one of these
examples, regenerate it with `make sync-docs-diagnostics` and review the diff.

Static checks do not prove that an agent completes its task. Add an execution
test with [recorded model responses](../testing.md) for runnable agent examples.
