use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use time::OffsetDateTime;
use tokio::sync::Notify;

use super::cron::scheduler::Clock;

#[derive(Debug)]
pub(crate) struct MockClock {
    now: Mutex<OffsetDateTime>,
    notify: Notify,
}

impl MockClock {
    pub(crate) fn new(now: OffsetDateTime) -> Arc<Self> {
        Arc::new(Self {
            now: Mutex::new(now),
            notify: Notify::new(),
        })
    }

    pub(crate) async fn set(&self, now: OffsetDateTime) {
        *self.now.lock().expect("mock clock mutex poisoned") = now;
        self.notify.notify_waiters();
    }

    pub(crate) async fn advance(&self, duration: time::Duration) {
        let next = *self.now.lock().expect("mock clock mutex poisoned") + duration;
        self.set(next).await;
    }
}

#[async_trait]
impl Clock for MockClock {
    fn now(&self) -> OffsetDateTime {
        *self.now.lock().expect("mock clock mutex poisoned")
    }

    async fn sleep_until(&self, deadline: OffsetDateTime) {
        loop {
            if *self.now.lock().expect("mock clock mutex poisoned") >= deadline {
                return;
            }
            self.notify.notified().await;
        }
    }
}
