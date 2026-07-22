# Windows CI lane speed experiments

Two independently-toggleable experiments for the `Rust on Windows (build +
smoke test)` job in `.github/workflows/ci.yml` and its cache writer,
`.github/workflows/windows-nightly.yml`. Each is gated by a repo variable and
defaults **off**, so merging this branch changes nothing until a variable is
set. Attribute wins by flipping one variable at a time.

## Baseline

Measured on run `29887103230` (push to `main`, full-workspace compile):

| Step | Wall time |
| --- | --- |
| Set up job + checkout + rust-toolchain + rust-cache restore + install nextest | ~48s |
| **Compile Windows tests and run unit slice** | **24m34s** |
| Smoke test harn run | ~1s |
| Cache save (consumer is restore-only) + cleanup | ~7s |
| **Total** | **25m34s** |

The compile+link step is >96% of wall time, so both experiments target it.

There are two cost regimes:

- **Cold path (21-26m):** crates-touching PRs and most `main` pushes land in a
  different `workspace-windows` cache namespace than the last nightly (the
  `source-key` fingerprints `crates/**`), so they compile the dep graph from
  scratch. Dominated by rustc codegen and the filesystem writes it emits.
- **Warm path (~6:21):** workflow/script-only PRs restore the nightly cache and
  become link-bound, exactly like the Linux warm lanes that motivated mold.

## Experiment A — rust-lld linker (`HARN_WINDOWS_LLD`)

**Change:** job-level `RUSTFLAGS` gains `-Clinker=rust-lld.exe` when
`vars.HARN_WINDOWS_LLD == 'on'`. This drives rustc at the toolchain-bundled
`rust-lld` (rustc injects the `lld-link` flavor for the msvc target) instead of
MSVC `link.exe`. It is the Windows analog of the `-Clink-arg=-fuse-ld=mold`
link-arg the Linux `rust-builds` lane already uses.

**Why it is not a no-op:** rust-lld became the *default* linker only on
`x86_64-unknown-linux-gnu` in Rust 1.90. `x86_64-pc-windows-msvc` still defaults
to `link.exe` on 1.95, and `-Clinker-features=+lld` is stable on `linux-gnu`
only — so the Linux flag cannot express this on Windows.

**Predicted effect:** biggest on the warm/link-bound path; also speeds the
incremental relinks and the final link of each test binary + `harn.exe` on the
cold path. Community reports ~2x full-build and ~5x incremental-link
improvements swapping link.exe for LLD on Windows. Realistic here: a meaningful
cut on warm runs, a smaller single-digit-percent cut on cold runs.

**Confidence:** high on mechanism, medium on magnitude (untested on this graph).
Note `-Clinker-features=+lld` is nightly-gated on `x86_64-pc-windows-msvc` (stable
only on `linux-gnu`), so the `-Clinker=` linker override is the correct stable
path — not the linux feature flag.

**Fallbacks if the runner cannot resolve the linker:** the staged value
`-Clinker=rust-lld.exe` uses the toolchain-bundled rust-lld (no install; rustc
resolves it and injects the `-flavor link` msvc mode). If that fails to resolve,
try `-Clinker=lld-link.exe` (the LLVM-shipped binary; verify it is on the
windows-2025 runner PATH first) or a full path to `lld-link.exe`. Edit the one
`RUSTFLAGS` line in both workflows identically.

## Experiment B — ReFS Dev Drive (`HARN_WINDOWS_DEVDRIVE`)

**Change:** when `vars.HARN_WINDOWS_DEVDRIVE == 'on'`, `samypr100/setup-dev-drive`
creates a 25 GB ReFS Dev Drive and remaps `CARGO_HOME` and `CARGO_TARGET_DIR`
onto it *before* the toolchain and cache steps run.

**Why:** the many small object/rmeta writes a cold compile emits are the classic
Windows CI filesystem tax. A Dev Drive uses ReFS with performance mode and
copy-on-write, which cuts the I/O portion of that tax. `windows-2025` dropped its
`D:` drive on 2025-07-14 and ships no pre-created Dev Drive, so it is created
in-job.

**Predicted effect: smaller than the laptop-scale numbers, and lower confidence
than Experiment A.** Much of Dev Drive's headline win is deferred antivirus
scanning — but Windows Defender real-time scanning is *already disabled* on
GitHub-hosted runners (uv PR #3522 measured this on real CI), so only the
ReFS-IO portion remains. Expect a modest cold-path improvement at best, possibly
neutral. Treat this as the secondary, exploratory lever; ship Experiment A
first.

**Confidence:** low-to-medium. Caveats to watch in the A/B:

- **rust-cache may silently miss the relocated target.** rust-cache's `target`
  handling is workspace-relative; the staged change points `CARGO_TARGET_DIR`
  *off-workspace* onto the Dev Drive. Before trusting any number, read the
  rust-cache step log and confirm it actually restores/saves the relocated
  target — a silent miss would make every run cold and erase the point. If it
  misses, switch to the safer variant: check the whole workspace out *onto* the
  Dev Drive so `./target` keeps its expected workspace-relative path and
  rust-cache works unchanged (more invasive — changes the checkout path and
  every step's working dir).
- **Drive-letter stability.** `setup-dev-drive` assigns the next free letter
  (usually `E:`). Rust artifacts embed absolute paths; if the letter varies
  run-to-run, incremental/fingerprint reuse from the restored cache can be
  invalidated. Confirm the letter is stable across two runs.
- **Governance.** This adds a new third-party action. It is pinned by commit SHA
  (`30f0f98…` = v4.0.0), but confirm it passes `actions-vulnerability-audit.yml`
  / `supply-chain.yml` before relying on it.
- **Disk headroom.** The 25 GB VHDX is backed by `C:` free space; shrink if a
  runner image tightens.

**Lower-risk alternative** (not staged): since Defender is already off on GH
runners the AV-exclusion trick buys little there, but if a future move to
self-hosted Windows runners (where Defender *is* active) is on the table, keep
the target dir in-workspace and add a Defender exclusion via
`Add-MpPreference -ExclusionPath "$CARGO_TARGET_DIR"` in a PowerShell step — no
third-party action, rust-cache path untouched.

## How to A/B

Each experiment's writer (`windows-nightly.yml:nextest`) and consumer
(`ci.yml:windows`) read the **same** repo variable, so they stay byte-identical —
required by the shared-cache policy in `scripts/check_ci_cache_policy.harn`,
which compares the two lanes' `RUSTFLAGS` and pre-cache step sequence.

1. Record 2 baseline runs with both variables unset (or `off`). Trigger the
   `windows` job via a Windows-sensitive PR, or dispatch `windows-nightly.yml`.
2. Set one variable to `on`
   (`gh variable set HARN_WINDOWS_LLD --body on --repo burin-labs/harn`).
3. Because `RUSTFLAGS` (Experiment A) and `CARGO_TARGET_DIR` (Experiment B) are
   part of rust-cache's environment hash, the first run under the new setting is
   cold. **Dispatch `windows-nightly.yml` once to re-seed the cache** before
   measuring, mirroring the post-merge acceptance protocol in PR #5130.
4. Compare the `Compile Windows tests and run unit slice` step wall time over 2
   warm runs each via
   `gh api repos/burin-labs/harn/actions/runs/<id>/jobs`.
5. Flip back to `off` and repeat for the other variable to attribute each win
   independently.

## Ruled out (history already refuted these)

- **`CARGO_PROFILE_DEV_DEBUG=line-tables-only` on Windows** — PRs #2127/#2128/
  #2129 measured it *regressed* Windows compile time (E2 16:22, E3 9:47 vs E1
  6:21 warm). Not applied. `profile.test` already carries `debug = 0`.
- **Loosening the `source-key` (dropping `crates/**`)** — would share the nightly
  cache with crates-touching PRs and kill most cold builds, but PR #5221 added
  the `source-key` specifically to stop the Windows target cache from serving
  stale embedded stdlib bytes across revisions (a real CLI-compat break that
  cargo's mtime fingerprint missed after a cache restore). Left intact; the cold
  build is the deliberate price of embedded-stdlib cache correctness.
- **sccache on Windows** — PRs #2114/#2131 measured 0% hit rate plus
  `os error 10054` upload failures; #5130 proved the runner-local daemon reached
  zero PR hits. Both Windows lanes stay on plain rustc.
- **`-Clinker-features=+lld`** — stable on `x86_64-unknown-linux-gnu` only;
  cannot select rust-lld on `x86_64-pc-windows-msvc`. Experiment A uses
  `-Clinker=rust-lld.exe` instead.

## Sources

- rust-lld default only on linux-gnu in 1.90:
  https://blog.rust-lang.org/2025/09/01/rust-lld-on-1.90.0-stable
- `linker-features` stable on linux-gnu only:
  https://doc.rust-lang.org/rustc/codegen-options/index.html
- LLD on Windows msvc, measured speedups:
  https://dsincl12.medium.com/faster-rust-builds-on-windows-7a7662c16f9
- `windows-2025` D: drive removed:
  https://github.com/actions/runner-images/issues/12416
- Dev Drive on GitHub runners for I/O (pip):
  https://github.com/pypa/pip/pull/13123
- Defender already off on GH runners; measured Dev Drive CI effect (uv):
  https://github.com/astral-sh/uv/pull/3522
- setup-dev-drive action:
  https://github.com/marketplace/actions/setup-dev-drive
