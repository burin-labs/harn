- **Ranges are `a..b` and `a..=b`.** `a..b` excludes the end and `a..=b`
  includes it, as in Rust. The `a to b` and `a to b exclusive` spellings
  were removed and now report `HARN-PAR-007` with the replacement. Ranges
  print in the same form (`1..=5`, `0..3`).
- **`??` binds like Swift's.** It is now looser than arithmetic and range
  formation and tighter than comparison, so `count ?? 0 + 1` is
  `count ?? (0 + 1)` (it was `(count ?? 0) + 1`) and `count ?? 0 == 1` is
  unchanged. Range formation moved too: it binds looser than `+`/`-` and does
  not chain, so `0..n - 1` is `0..(n - 1)` and `x in 0..n` tests membership.
- **Domain words are identifiers again.** `to` and `exclusive` are no longer
  keywords, and `from`, `tool`, `select`, `emit`, `require`, `deadline`,
  `mutex`, `skill`, and `eval_pack` are contextual: they introduce their
  construct only in position, so `let tool = ...` and `config.from` work.
- **Migration:** `harn fix --apply --migrate-operator-syntax <path>` rewrites
  every range and inserts parentheses wherever the new precedence would
  regroup an expression, then re-parses each file and refuses any rewrite
  whose tree differs. Run it once per tree.
