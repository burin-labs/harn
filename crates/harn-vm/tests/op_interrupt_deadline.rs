use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use harn_vm::op_interrupt::{install, installed, requested, with_deadline};

#[test]
fn scoped_deadline_preserves_live_parent_cancellation() {
    let token = Arc::new(AtomicBool::new(false));
    let _parent = install(Some(token.clone()), None);
    {
        let _operation = with_deadline(Instant::now() + Duration::from_hours(1));
        assert!(!requested());
        token.store(true, Ordering::SeqCst);
        assert!(
            requested(),
            "nested operation must observe parent cancellation"
        );
    }
    assert!(
        requested(),
        "dropping the operation must restore its parent"
    );
}

#[test]
fn scoped_deadline_cannot_extend_an_expired_parent() {
    let _parent = install(None, Some(Instant::now()));
    let _operation = with_deadline(Instant::now() + Duration::from_hours(1));
    assert!(requested());
}

#[test]
fn scoped_deadline_expires_and_restores_a_live_parent() {
    let parent = install(None, Some(Instant::now() + Duration::from_hours(1)));
    {
        let _operation = with_deadline(Instant::now());
        assert!(requested());
    }
    assert!(installed());
    assert!(!requested());
    drop(parent);
    assert!(!installed());
    {
        let _operation = with_deadline(Instant::now());
        assert!(
            requested(),
            "a top-level operation must also install its deadline"
        );
    }
    assert!(!installed());
}
