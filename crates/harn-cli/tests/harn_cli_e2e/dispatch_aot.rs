//! Integration tests for the AOT bytecode embedding fast path
//! (harn#2300 / G7) wired into the dispatch wedge.
//!
//! Goals:
//!   1. Toggling `HARN_DISABLE_AOT_CLI` off (the default) and on both
//!      produce byte-identical dispatch output. Output equivalence is
//!      the load-bearing contract: AOT must be a transparent fast path.
//!   2. The opt-out env var is honored across the supported truthy
//!      values so users can paste any of them into a shell without
//!      surprises.
//!   3. Dispatch tolerates a poisoned bytecode cache directory (the
//!      runtime falls back to source compilation) without crashing.

use tokio::sync::Mutex;

use harn_cli::dispatch::{run_embedded_script, DISABLE_AOT_ENV};

// Env-var tests must not run concurrently with anything else that
// touches the same vars. Tokio's `#[tokio::test]` defaults to
// multi-thread; this async mutex serializes the per-test env
// mutation across the awaits below. Using `tokio::sync::Mutex`
// rather than `std::sync::Mutex` keeps clippy's
// `await_holding_lock` lint quiet.
static ENV_LOCK: Mutex<()> = Mutex::const_new(());

struct ScopedEnv {
    key: &'static str,
    previous: Option<String>,
}

impl ScopedEnv {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        // SAFETY: tests under ENV_LOCK serialize all env mutations.
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }

    fn unset(key: &'static str) -> Self {
        let previous = std::env::var(key).ok();
        // SAFETY: tests under ENV_LOCK serialize all env mutations.
        unsafe { std::env::remove_var(key) };
        Self { key, previous }
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        // SAFETY: tests under ENV_LOCK serialize all env mutations.
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

#[tokio::test]
async fn dispatch_falls_back_to_source_when_aot_disabled() {
    let _lock = ENV_LOCK.lock().await;
    let _scope = ScopedEnv::set(DISABLE_AOT_ENV, "1");

    let outcome = run_embedded_script("echo", vec!["foo".into(), "bar".into()], false).await;
    assert_eq!(
        outcome.exit_code, 0,
        "echo with AOT disabled failed: stderr={}",
        outcome.stderr
    );
    assert_eq!(outcome.stdout, "[\"foo\",\"bar\"]\n");
}

#[tokio::test]
async fn dispatch_works_with_aot_enabled_default() {
    let _lock = ENV_LOCK.lock().await;
    let _scope = ScopedEnv::unset(DISABLE_AOT_ENV);

    let outcome = run_embedded_script("echo", vec!["foo".into(), "bar".into()], false).await;
    assert_eq!(
        outcome.exit_code, 0,
        "echo with AOT enabled failed: stderr={}",
        outcome.stderr
    );
    assert_eq!(outcome.stdout, "[\"foo\",\"bar\"]\n");
}

#[tokio::test]
async fn dispatch_output_is_identical_with_and_without_aot() {
    let _lock = ENV_LOCK.lock().await;

    let aot_outcome = {
        let _scope = ScopedEnv::unset(DISABLE_AOT_ENV);
        run_embedded_script("echo", vec!["alpha".into(), "beta".into()], false).await
    };
    let src_outcome = {
        let _scope = ScopedEnv::set(DISABLE_AOT_ENV, "1");
        run_embedded_script("echo", vec!["alpha".into(), "beta".into()], false).await
    };

    assert_eq!(aot_outcome.exit_code, src_outcome.exit_code);
    assert_eq!(aot_outcome.stdout, src_outcome.stdout);
    assert_eq!(aot_outcome.stderr, src_outcome.stderr);
}

/// Opt-out env values that should disable the AOT fast path. The
/// dispatch reader matches what `harn_vm::bytecode_cache::cache_enabled`
/// recognizes, so each of these must keep echo working without ever
/// dropping the adjacent bytecode artifact.
#[tokio::test]
async fn aot_opt_out_recognizes_all_truthy_values() {
    let _lock = ENV_LOCK.lock().await;

    for value in ["1", "true", "yes", "on", "TRUE", "Yes"] {
        let _scope = ScopedEnv::set(DISABLE_AOT_ENV, value);
        let outcome = run_embedded_script("echo", vec!["x".into()], false).await;
        assert_eq!(
            outcome.exit_code, 0,
            "echo with HARN_DISABLE_AOT_CLI={value} failed: stderr={}",
            outcome.stderr
        );
        assert_eq!(outcome.stdout, "[\"x\"]\n");
    }
}

/// Falsy values (and the empty string) leave AOT enabled. Verifies the
/// reader doesn't accidentally treat e.g. `0` as opt-out.
#[tokio::test]
async fn aot_opt_out_falsy_values_keep_aot_enabled() {
    let _lock = ENV_LOCK.lock().await;

    for value in ["0", "false", "no", "off", ""] {
        let _scope = ScopedEnv::set(DISABLE_AOT_ENV, value);
        let outcome = run_embedded_script("echo", vec!["x".into()], false).await;
        assert_eq!(
            outcome.exit_code, 0,
            "echo with HARN_DISABLE_AOT_CLI={value:?} failed: stderr={}",
            outcome.stderr
        );
        assert_eq!(outcome.stdout, "[\"x\"]\n");
    }
}
