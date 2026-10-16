use super::*;

#[test]
fn a_held_stdlib_warm_lock_excludes_a_sibling_warm_until_released() {
    // Sharded test runs start several warming processes over one disk cache.
    // A second warm must wait for the first rather than compile the same
    // catalog beside it; a zero deadline makes that wait observable at once.
    let _guard = cache_test_guard();
    let cache = tempfile::tempdir().expect("temp cache dir");
    let previous = std::env::var_os(crate::bytecode_cache::CACHE_DIR_ENV);
    std::env::set_var(crate::bytecode_cache::CACHE_DIR_ENV, cache.path());
    let no_wait = std::time::Duration::ZERO;

    let holder = acquire_stdlib_warm_lock(no_wait).expect("an uncontended warm takes the lock");
    assert!(
        acquire_stdlib_warm_lock(no_wait).is_none(),
        "a sibling warm must not take the lock while another holds it"
    );
    drop(holder);
    assert!(
        acquire_stdlib_warm_lock(no_wait).is_some(),
        "the lock is free once its holder drops it"
    );

    match previous {
        Some(value) => std::env::set_var(crate::bytecode_cache::CACHE_DIR_ENV, value),
        None => std::env::remove_var(crate::bytecode_cache::CACHE_DIR_ENV),
    }
}
