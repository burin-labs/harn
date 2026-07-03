- **Ternary branch merging matches if/else expressions.** `cond ? 1 :
  unreachable(…)` infers `int` (the `never` arm collapses) and nested unions
  flatten/dedup instead of producing `Union[Union[…]]` shapes that defeated
  downstream narrowing.
- **Aliased collection receivers keep their element/value types across all
  methods.** With `type Env = dict<string, string>`, methods like
  `.map_values()`, `.merge()`, `.window()`, and `.iter()` now see through the
  alias the way `.values()`/`.keys()` already did.
- **The falsy branch of `schema_is(x, S)` no longer over-narrows.** Subtracting
  a literal schema (`"a"`) from `string | int` kept only `int`, wrongly
  dropping the whole `string` member; members are now subtracted only when
  every value of the member matches the schema.
