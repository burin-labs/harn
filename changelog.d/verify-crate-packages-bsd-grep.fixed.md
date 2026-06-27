- **Release packaging guard works on BSD/macOS grep again.** The
  `verify_crate_packages.sh` checks that fail when a packaged `harn-stdlib` /
  `harn-modules` crate still contains workspace-relative consumer includes used
  GNU-only BRE alternation (`\(vm\|modules\)`). On BSD/macOS grep `\|` matches
  literally, so the guards silently never matched and passed even on a broken
  package — a no-op on the primary dev platform where `release_gate.sh` is run.
  Switched both to portable ERE (`grep -RE '(vm|modules)'` /
  `'(vm|stdlib)'`); behavior on GNU grep is unchanged.
