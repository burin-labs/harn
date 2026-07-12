Two module/loop ergonomics gaps where `harn check` passed but behavior was
silently wrong are now loud and correct: (1) a `for (a, b)` pair pattern over a
non-`Pair` item (e.g. `for (i, x) in list.enumerate()`, whose items are
`{index, value}` dicts) previously bound both names to `nil` -- it now fails
loudly, naming the supported forms (`for {index, value} in list.enumerate()`,
`for [a, b] in list.zip(...)`, or wrap with `iter(...)`); (2) when an imported
module fails to lex/parse, `harn check` on a consumer now surfaces that module's
real error anchored at the `import` (`HARN-MOD-007`) instead of mislabeling every
imported symbol as "undefined" at the consumer's call site.
