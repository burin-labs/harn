**The feature matrix compares more systems without becoming a scroll bar.**
`docs/src/feature-matrix.md` now renders from one typed source
(`website/src/data/comparison.ts`) through a `{{#comparison-matrix}}`
directive, and readers pick which systems to compare against from a legend
above the table. BAML and Flue join the roster. Every column ships in the
prerendered HTML and in the Markdown projection agents read, so the picker only
narrows what is on screen; a reader without JavaScript still gets the whole
comparison. The roster, the ratings, and the reference list used to be three
hand-kept lists on the same page.
