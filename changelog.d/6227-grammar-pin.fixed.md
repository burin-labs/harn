A grammar version that Harn deliberately holds back can no longer be
re-proposed and quietly re-blessed. `tree-sitter-swift` is pinned at `=0.7.2`
because 0.7.3 regressed to a root missing-node on the optional-chain fixture in
the fitness corpus, but nothing carried that decision anywhere a bot or a later
reader would look: the pin had no comment, Dependabot cannot read one anyway,
and no open issue tracked undoing it. The bump has been proposed three times and
merged once.

`.github/dependabot.yml` now ignores exactly the known-bad version, so a future
release that fixes the regression still arrives as a normal update, and #6227
tracks the unpin.

`make check-grammar-fitness` also told operators the wrong thing in the case
that mattered. A stale receipt has two causes wanting opposite fixes: editing
the corpus moves the corpus digest and regenerating is the whole fix, while a
grammar package moving versions moves that language's artifact digest and
regenerating only re-stamps fitness onto an artifact nothing has re-proven. Both
produced one message recommending `make gen-grammar-fitness`. The check now
names each language whose grammar moved, with both versions and both artifact
digests, and says to revert or re-run the corpus test instead.
