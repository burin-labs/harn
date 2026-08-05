# Release binary-size policy

`.github/release-binary-size-policy.json` is the source of truth for release
binary-size admission. It answers two different questions with two different
numbers.

## The distribution fuse

`distribution_fuse_bytes` is a fail-closed absolute ceiling: an artifact above
it is not distributable, whatever produced it. It is an emergency fuse, not a
ratchet, so the contract requires it to sit between 10% and 15% above the
accepted baseline. Tighter and it becomes a release-day byte cliff — the
schema-1 policy this replaced allowed at most 4 MiB of headroom above a ~222 MiB
baseline, which moved the ceiling 212 → 213 → 216 → 218 → 220 MiB inside one
release line, each raise its own PR and its own exact release build after the
useful work was already certified. Looser and it stops being a fuse.

Raising the fuse is a deliberate policy decision, taken rarely, alongside a
baseline refresh.

## The growth signal

`growth.warn_bytes` and `growth.warn_percent_hundredths` are the early
complexity signal. A crossing needs **both**: an absolute floor, so a few
megabytes on a 200+ MiB artifact is not treated as news, and a ratio, so the
same thresholds still mean something on a smaller target.
`warn_percent_hundredths` is an integer per-ten-thousand — 200 is 2.00% — so the
contract carries no float comparison.

A crossing fails the release build unless an `accepted_growth` entry covers it.
An entry names `against_baseline_sha`, the byte allowance, and a short reason.
It is scoped to the baseline it was written against rather than to the commit
that caused the growth, because a contributor cannot predict the squashed `main`
SHA their PR will become — but they do know which baseline they measured
against. Validation rejects an entry whose `against_baseline_sha` is not the
current baseline, so refreshing the baseline forces every stale acceptance to be
dropped in the same change.

## Comparability

Raw bytes cannot classify causality. Between v0.10.52 candidates with identical
source and toolchain, moving x86_64 release codegen units from 16 to 8 shed
8,407,168 bytes — codegen duplication and layout, not eight megabytes of
product. Subtracting two differently-built artifacts produces a number with no
causal meaning.

The baseline therefore carries a `build_identity`: profile, codegen units, LTO,
strip, rustc, and whether the AOT payload is embedded. The check observes the
same fields for the build in front of it (from `CARGO_PROFILE_RELEASE_*` and
`rustc -vV`, falling back to `[profile.release]` in the workspace manifest). If
any diverge, the report says which, classifies the comparison as
`not-comparable`, and draws no growth conclusion. The fuse still applies.

`aot_embedded` is a boolean, not a byte count, on purpose: whether the payload
is compiled in is a build-identity question, while how large it has become is
product growth this policy exists to report.

## Attribution

`scripts/check_binary_size.harn` writes `binary-size.txt` with the fuse state,
the baseline delta in bytes and hundredths of a percent, the comparability
verdict, and both build identities, so a blocked release carries its own
attribution instead of costing a second exact release build. The release job
also emits `elf-sections.txt` (`size -A -d`) and a `cargo bloat --crates`
report.

Given a baseline `size -A -d` table via `--baseline-sections`, the report adds
per-section deltas and classifies them as `codegen-layout`, `content`, or
`mixed`: `.text`, `.eh_frame`, `.eh_frame_hdr`, and `.gcc_except_table` move
together on a layout change, while an embedded asset lands in `.rodata` and
friends. The release workflow does not yet carry a stored baseline table across
runs, so the delta is available to an operator comparing two archived reports
rather than automatically.

## Where each level runs

`.github/release-binary-size-policy.jq` is the bootstrap validator, used before
the release job installs or builds Harn. It decodes the same closed contract and
emits the fuse in bytes. `scripts/release_binary_size_policy.harn` owns the
typed contract and the verdict; `scripts/check_binary_size.harn` applies it. The
focused PR gate exercises both validators against unknown fields, invalid
metadata, duplicate targets, topology failures, both fuse bounds, and a stale
acceptance.

In `build-release-binaries.yml` the fuse is a pure-bash arithmetic step, so it
holds even when the binary it just measured cannot run. The growth signal is a
separate step that runs the freshly built `harn` against the policy. Collapsing
them back into one step is the regression this split exists to prevent.

`--fuse-mb`, `BINARY_SIZE_FUSE_MB`, and `BINARY_SIZE_FUSE_BYTES` are explicit
local or recovery overrides. They move the fuse only: the growth signal always
compares against the recorded baseline, so overriding the ceiling cannot
silently retire the early signal too.

## Refreshing the baseline

When a release establishes a new accepted size, record the candidate version,
source SHA, byte count, observation time, and build identity from that Actions
run, and drop any `accepted_growth` entries written against the previous
baseline. Do not estimate from a local binary built for another target.

Only x86_64 Linux is budgeted today. Adding another target entry is data
preparation, not activation: also add the matching workflow gate and report
surface before treating that target as enforced.
