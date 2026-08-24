**The comparison page compares more systems without becoming a scroll bar.**
`docs/src/how-harn-compares.md` now renders from one typed source
(`website/src/data/comparison.ts`) through a `{{#comparison-matrix}}`
directive, and readers pick which systems to compare against from a legend
above the table. BAML and Flue join the roster. Every column ships in the
prerendered HTML and in the Markdown projection agents read, so the picker only
narrows what is on screen; a reader without JavaScript still gets the whole
comparison. The roster, the ratings, and the reference list used to be three
hand-kept lists on the same page.

Three rows join the table: sandboxed-by-default execution, signed and versioned
packages, and install size. Each compared system that has a term-by-term
vocabulary map now links to it from the table, so a reader who has decided the
comparison is in Harn's favour has somewhere to go next.
