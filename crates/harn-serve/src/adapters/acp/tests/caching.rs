//! Compile-cache and VM-baseline caching for ACP sessions.
//!
//! These drive the caching helpers directly rather than spinning a full ACP
//! server, so the assertions stay on cache mechanics; the end-to-end paths are
//! exercised by the hot-reload test in the `commands` submodule.
//!
//! Split out of `tests.rs`, which had grown past the source-length ratchet's
//! cap; the tests are unchanged by the move.

use super::*;

/// Compile cache: re-issuing a `session/prompt` on the same pipeline file
/// must serve the bytecode from cache. Touching the file (advancing mtime)
/// or switching the target pipeline name must invalidate the slot. This
/// drives the helper directly rather than spinning a full ACP server so the
/// assertion stays focused on the cache mechanics — the end-to-end path is
/// exercised by the existing hot-reload test in the `commands` submodule.
#[test]
fn compile_pipeline_cached_serves_cached_chunk_until_mtime_advances() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pipeline_path = dir.path().join("p.harn");
    let initial = "pipeline main() { __io_println(\"first\") }\n";
    std::fs::write(&pipeline_path, initial).expect("write initial");

    let mut server = AcpServer::new(AcpServerConfig::new(Some(
        pipeline_path.to_string_lossy().to_string(),
    )));

    let (_chunk, hit1) = server
        .compile_pipeline_cached(initial, Some(pipeline_path.as_path()), None)
        .expect("first compile");
    assert!(!hit1, "first compile must miss the cache");

    let (_chunk, hit2) = server
        .compile_pipeline_cached(initial, Some(pipeline_path.as_path()), None)
        .expect("second compile");
    assert!(hit2, "second compile of unchanged source must hit");

    // Switching `target_pipeline` invalidates the slot — a named compile
    // produces a different chunk than the default-entry compile.
    let named_source = "@command(name: \"alpha\") pipeline alpha() { __io_println(\"alpha\") }\n\
                       pipeline main() { __io_println(\"main\") }\n";
    std::fs::write(&pipeline_path, named_source).expect("write named");
    // Force mtime advance with a deterministic far-future literal so the
    // test doesn't read the wall clock (banned by `make lint-test-patterns`).
    // 2_000_000_000 = 2033-05-18, comfortably after any plausible CI clock
    // and well past the whole-second rounding some filesystems apply to
    // fresh writes.
    let bumped = filetime::FileTime::from_unix_time(2_000_000_000, 0);
    filetime::set_file_mtime(&pipeline_path, bumped).expect("bump mtime");
    let (_chunk, hit3) = server
        .compile_pipeline_cached(named_source, Some(pipeline_path.as_path()), Some("alpha"))
        .expect("named compile");
    assert!(
        !hit3,
        "different mtime + target_pipeline must miss the previous slot"
    );

    let (_chunk, hit4) = server
        .compile_pipeline_cached(named_source, Some(pipeline_path.as_path()), Some("alpha"))
        .expect("named compile second");
    assert!(hit4, "repeated named compile must hit");
}

/// Inline-mode prompts (no `source_path`) are not cached — they're one-off
/// by construction and caching them would just bloat memory.
#[test]
fn compile_pipeline_cached_does_not_cache_inline_prompts() {
    let mut server = AcpServer::new(AcpServerConfig::new(None));
    let source = "pipeline main() { __io_println(\"inline\") }\n";
    let (_chunk, hit1) = server
        .compile_pipeline_cached(source, None, None)
        .expect("first inline compile");
    assert!(!hit1);
    let (_chunk, hit2) = server
        .compile_pipeline_cached(source, None, None)
        .expect("second inline compile");
    assert!(
        !hit2,
        "inline-mode compiles must not be cached (per-turn source is dynamic)"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn vm_baseline_cached_serves_file_backed_context_until_key_changes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pipeline_path = dir.path().join("baseline.harn");
    let source = "pipeline main() { __io_println(\"baseline\") }\n";
    std::fs::write(&pipeline_path, source).expect("write pipeline");

    let mut server = AcpServer::new(AcpServerConfig::new(Some(
        pipeline_path.to_string_lossy().to_string(),
    )));
    let (_baseline, hit1, _ms1) = server
        .prepare_vm_baseline_cached(
            source,
            Some(pipeline_path.as_path()),
            None,
            dir.path(),
            dir.path(),
            "code",
        )
        .await
        .expect("first prepare");
    assert_eq!(hit1, Some(false), "first prepare must fill the cache");

    let (_baseline, hit2, _ms2) = server
        .prepare_vm_baseline_cached(
            source,
            Some(pipeline_path.as_path()),
            None,
            dir.path(),
            dir.path(),
            "code",
        )
        .await
        .expect("second prepare");
    assert_eq!(hit2, Some(true), "unchanged file-backed context must hit");

    let (_baseline, hit3, _ms3) = server
        .prepare_vm_baseline_cached(
            source,
            Some(pipeline_path.as_path()),
            Some("review"),
            dir.path(),
            dir.path(),
            "code",
        )
        .await
        .expect("target prepare");
    assert_eq!(
        hit3,
        Some(false),
        "target pipeline is part of baseline invalidation"
    );

    let (_baseline, hit4, _ms4) = server
        .prepare_vm_baseline_cached(
            source,
            Some(pipeline_path.as_path()),
            Some("review"),
            dir.path(),
            dir.path(),
            "plan",
        )
        .await
        .expect("mode prepare");
    assert_eq!(
        hit4,
        Some(false),
        "ACP mode is part of baseline invalidation"
    );

    let other_root = tempfile::tempdir().expect("other project root");
    let (_baseline, hit5, _ms5) = server
        .prepare_vm_baseline_cached(
            source,
            Some(pipeline_path.as_path()),
            Some("review"),
            dir.path(),
            other_root.path(),
            "plan",
        )
        .await
        .expect("different project-root prepare");
    assert_eq!(
        hit5,
        Some(false),
        "session project root is part of baseline invalidation"
    );

    let (baseline, hit6, ms6) = server
        .prepare_vm_baseline_cached(source, None, None, dir.path(), dir.path(), "code")
        .await
        .expect("inline prepare");
    assert!(baseline.is_none());
    assert_eq!(hit6, None);
    assert_eq!(ms6, 0);
}
