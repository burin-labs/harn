//! Workspace scans cannot attribute effects while dispatches overlap.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

#[derive(Default)]
pub(super) struct Dispatches(Mutex<Vec<Weak<AtomicBool>>>);

impl Dispatches {
    pub(super) fn begin(&self) -> Dispatch {
        let mut active = self.0.lock().expect("tape dispatch lock poisoned");
        let mut overlapping = false;
        active.retain(|entry| {
            if let Some(entry) = entry.upgrade() {
                entry.store(true, Ordering::SeqCst);
                overlapping = true;
                true
            } else {
                false
            }
        });
        let flag = Arc::new(AtomicBool::new(overlapping));
        active.push(Arc::downgrade(&flag));
        Dispatch(flag)
    }
}

pub(super) struct Dispatch(Arc<AtomicBool>);

impl Dispatch {
    pub(super) fn overlapped(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}
