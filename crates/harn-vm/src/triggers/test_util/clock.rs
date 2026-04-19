use std::cell::RefCell;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration as StdDuration, Instant};

use async_trait::async_trait;
use time::OffsetDateTime;
use tokio::sync::Notify;

use crate::connectors::cron::scheduler::Clock;

thread_local! {
    static MOCK_CLOCK_STACK: RefCell<Vec<Arc<MockClock>>> = const { RefCell::new(Vec::new()) };
}

fn process_start() -> &'static Instant {
    static PROCESS_START: OnceLock<Instant> = OnceLock::new();
    PROCESS_START.get_or_init(Instant::now)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClockInstant(StdDuration);

impl ClockInstant {
    pub fn duration_since(self, earlier: Self) -> StdDuration {
        self.0.saturating_sub(earlier.0)
    }

    pub fn as_millis(self) -> u128 {
        self.0.as_millis()
    }
}

pub struct ClockOverrideGuard;

impl Drop for ClockOverrideGuard {
    fn drop(&mut self) {
        MOCK_CLOCK_STACK.with(|slot| {
            slot.borrow_mut().pop();
        });
    }
}

#[derive(Debug)]
pub struct MockClock {
    now: Mutex<OffsetDateTime>,
    monotonic: Mutex<StdDuration>,
    notify: Notify,
}

impl MockClock {
    pub fn new(now: OffsetDateTime) -> Arc<Self> {
        Arc::new(Self {
            now: Mutex::new(now),
            monotonic: Mutex::new(StdDuration::ZERO),
            notify: Notify::new(),
        })
    }

    pub fn monotonic_now(&self) -> ClockInstant {
        ClockInstant(
            *self
                .monotonic
                .lock()
                .expect("mock clock monotonic mutex poisoned"),
        )
    }

    pub async fn set(&self, now: OffsetDateTime) {
        let mut wall = self.now.lock().expect("mock clock mutex poisoned");
        let previous = *wall;
        *wall = now;
        drop(wall);

        if now > previous {
            let delta = now - previous;
            if let Ok(delta) = TryInto::<StdDuration>::try_into(delta) {
                let mut monotonic = self
                    .monotonic
                    .lock()
                    .expect("mock clock monotonic mutex poisoned");
                *monotonic += delta;
            }
        }

        self.notify.notify_waiters();
    }

    pub async fn advance(&self, duration: time::Duration) {
        let Ok(delta) = TryInto::<StdDuration>::try_into(duration) else {
            return;
        };
        self.advance_std(delta).await;
    }

    pub async fn advance_std(&self, duration: StdDuration) {
        if duration.is_zero() {
            self.notify.notify_waiters();
            return;
        }
        let delta =
            time::Duration::try_from(duration).expect("std duration should fit in time::Duration");
        let next = *self.now.lock().expect("mock clock mutex poisoned") + delta;
        self.set(next).await;
    }

    pub async fn advance_ticks(&self, ticks: u32, tick: StdDuration) {
        for _ in 0..ticks {
            self.advance_std(tick).await;
        }
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

pub fn install_override(clock: Arc<MockClock>) -> ClockOverrideGuard {
    MOCK_CLOCK_STACK.with(|slot| {
        slot.borrow_mut().push(clock);
    });
    ClockOverrideGuard
}

pub fn active_mock_clock() -> Option<Arc<MockClock>> {
    MOCK_CLOCK_STACK.with(|slot| slot.borrow().last().cloned())
}

pub fn now_utc() -> OffsetDateTime {
    active_mock_clock()
        .map(|clock| clock.now())
        .unwrap_or_else(OffsetDateTime::now_utc)
}

pub fn now_ms() -> i64 {
    now_utc().unix_timestamp_nanos() as i64 / 1_000_000
}

pub fn instant_now() -> ClockInstant {
    active_mock_clock()
        .map(|clock| clock.monotonic_now())
        .unwrap_or_else(|| ClockInstant(process_start().elapsed()))
}
