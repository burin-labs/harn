# Markdown stdlib

`std/markdown` parses Markdown and MDX for documentation checks, search indexes,
and prose reports. It uses the runtime's `content` feature, which the Harn CLI
includes. Lean embedders must enable that feature.

## `parse(source, options)`

Returns a `MarkdownDocument`. `options.mdx` defaults to `false`; set it to `true`
to recognize JSX, imports, exports, and embedded expressions. Parsing doesn't
evaluate JavaScript or load imported modules. Invalid MDX structure throws.

| Field | Contents |
| --- | --- |
| `headings` | Heading level, rendered text, unique GitHub-style anchor, line, and offset |
| `links` | Resolved destination, image flag, line, and offset |
| `blocks` | Prose text, heading flag, and starting line for each block |
| `text` | Normalized visible text, including code and table contents |

Lines start at one. Offsets count UTF-8 bytes from zero. Links include inline
links, images, reference uses, and reference definitions. Code examples don't
create headings or links. Repeated headings get unique anchors, including
headings whose text already ends in a numbered suffix.

Prose blocks retain link labels and heading or list boundaries. They exclude
frontmatter, code, images, tables, HTML comments, and bare web addresses. MDX
mode also excludes import, export, and expression text. HTML extraction excludes
script and style contents.

```harn
import { parse } from "std/markdown"

fn main(harness: Harness) -> nil {
  const document = parse("# Example\n\nRead the [guide](./guide.md).")
  harness.stdio.println(document.headings[0]!.anchor)
  harness.stdio.println(document.links[0]!.destination)
}
```

This prints `example` and `./guide.md`.

## `reading_level(source, options)`

Returns a `ReadingLevel`, or `nil` when no English words remain. It uses the
same parser and options as `parse`.

| Field | Meaning |
| --- | --- |
| `words` | English word count |
| `sentences` | Sentence count, with each heading treated as one unit |
| `words_per_sentence` | Mean words per unit |
| `syllables_per_word` | Mean estimated syllables per word |
| `grade` | Flesch-Kincaid grade level |
| `ease` | Flesch reading ease |
| `longest` | Unit with the most words |

Scores estimate word and sentence length. They don't measure jargon,
correctness, or how well a page explains its subject.

`extract_text` from `std/document` uses the same Markdown and HTML extraction.
Pass `{source_format: "markdown"}` or `{source_format: "html"}` to select it.
