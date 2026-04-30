#![allow(dead_code)]

//! Subprocess harness shared by every harn-cli integration test binary.
//!
//! ## Why this module exists
//!
//! Until April 2026 the workspace nextest config capped the
//! `harn-subprocess` and `harn-cli-bin` test groups at `max-threads = 4`
//! to avoid macOS dyld/amfi cold-cache scheduler starvation: when N
//! tests simultaneously spawn a freshly-built `harn` debug binary for
//! the first time, the kernel page-ins, dyld closure construction, and
//! AMFI signature validation contend on the same shared resources, and
//! every individual subprocess startup balloons from ~300 ms to several
//! seconds. The tail of that cohort then trips nextest's 60 s
//! slow-test ceiling even though each test is fast in isolation.
//!
//! The `max-threads = 4` cap was a workaround, not a fix: it kept peak
//! cold-cohort size small enough that no individual subprocess starved,
//! at the cost of bounding wall-clock parallelism well below what the
//! host could otherwise sustain.
//!
//! ## What this module does
//!
//! Every test that spawns the `harn` binary obtains its `Command`
//! through [`harn_command`]. On first call within a given test-binary
//! process, a one-shot pre-warm runs `harn --version` synchronously
//! with a tight timeout. That single invocation:
//!
//! - Page-ins the binary's `__TEXT` segment and the dynamic libraries
//!   it links against (libsystem, sqlite3, openssl/aws-lc-rs, etc.).
//!   The macOS unified buffer cache holds these system-wide, so every
//!   *subsequent* spawn — within this test binary or any other test
//!   binary running on the same host — short-circuits the cold-cache
//!   path entirely.
//! - Triggers AMFI signature validation once, in isolation, instead of
//!   amplifying it across a parallel cohort.
//! - Forces dyld closure construction once, paying the cost on the
//!   warm-up path rather than on whichever test happens to spawn first
//!   in its cohort.
//!
//! Subsequent calls return the warmed binary path with no synchronization
//! overhead beyond a [`LazyLock`] read. The pre-warm is per-process, not
//! per-test, so a single test binary that spawns N children pays the
//! warm-up cost exactly once.
//!
//! With the pre-warm in place, parallel cohorts no longer trip the
//! 60 s ceiling, and the `max-threads = 4` cap can be relaxed.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

/// Maximum time the one-shot `harn --version` pre-warm is allowed to
/// take before we give up and fall through to per-test timing. The
/// happy-path warm-up on Apple Silicon dev hardware is sub-second; the
/// pre-warm is intentionally invoked from a directory with no `harn.toml`
/// so path-discovery walking doesn't pad the cold-start budget. We
/// match the workspace-wide `PROCESS_FAIL_FAST_TIMEOUT` (60 s, defined
/// in [`crate::test_util::timing`]) here because a heavily-loaded
/// developer box (concurrent worktree builds, Spotlight indexing, AMFI
/// signature daemon catch-up) can legitimately push the very first cold
/// spawn into double-digit seconds. Shorter would let environmental
/// noise turn the architectural fix into a flake source. A hung
/// pre-warm still surfaces as an actionable panic, not a silent hang.
const PREWARM_TIMEOUT: Duration = Duration::from_secs(60);

/// Resolved path to the `harn` debug binary, with the dyld + AMFI
/// caches warmed on first access. Always go through this rather than
/// `env!("CARGO_BIN_EXE_harn")` directly.
pub fn harn_binary() -> &'static Path {
    static WARMED: LazyLock<PathBuf> = LazyLock::new(|| {
        let path = PathBuf::from(env!("CARGO_BIN_EXE_harn"));
        prewarm(&path);
        path
    });
    WARMED.as_path()
}

/// Returns a `Command` builder for the warmed `harn` binary. Equivalent
/// to `Command::new(harn_binary())` but communicates intent at call
/// sites and serves as the canonical entry point for future
/// instrumentation (e.g. spawn-rate metering).
pub fn harn_command() -> Command {
    Command::new(harn_binary())
}

fn prewarm(path: &Path) {
    let started = Instant::now();
    // Run the warm-up from a directory that has no `harn.toml` so the
    // CLI's manifest-discovery walk doesn't inflate the cold-start
    // budget. From inside the harn repo, even `harn --version` can take
    // multiple seconds because the binary stat-walks ancestors looking
    // for a workspace manifest. `/` is universally safe: it has no
    // `harn.toml`, every test process can `chdir` to it, and the path
    // resolution for the spawn target itself is already absolute.
    let mut child = match Command::new(path)
        .current_dir("/")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => panic!(
            "harn-cli test prewarm: failed to spawn `{} --version`: {error}",
            path.display()
        ),
    };

    let deadline = started + PREWARM_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    panic!(
                        "harn-cli test prewarm: `{} --version` exited unsuccessfully: {status}",
                        path.display()
                    );
                }
                return;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "harn-cli test prewarm: `{} --version` exceeded {:?}; \
                         the dyld/AMFI cold-cache warm-up is the architectural \
                         predicate for relaxing max-threads — investigate before \
                         reverting the cap.",
                        path.display(),
                        PREWARM_TIMEOUT
                    );
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => panic!(
                "harn-cli test prewarm: failed to poll `{} --version`: {error}",
                path.display()
            ),
        }
    }
}
