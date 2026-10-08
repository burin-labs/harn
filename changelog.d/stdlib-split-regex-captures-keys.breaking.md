`split` now requires a separator in both forms. Previously `split(text)`
defaulted to `" "` while `text.split()` defaulted to `","`, so the two forms
disagreed; both now throw `split: separator is required`.

`regex_captures` now rejects a named group called `match`, `groups`, `start`,
`end`, or `line` with an `Invalid regex: named group ... collides with a
reserved regex_captures key` error. Previously the group silently overwrote
the built-in key, so `(?P<start>\d+)` made `.start` a string instead of the
match's character offset.

Migration: pass the separator explicitly (`split(text, " ")`,
`text.split(",")`) and rename colliding groups (for example `(?P<from>...)`).
