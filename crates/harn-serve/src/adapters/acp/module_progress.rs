use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use harn_vm::ModulePhaseStats;

use super::AcpBridge;

const PHASE: &str = "module_preparation";
const SCHEMA: &str = "harn.module_preparation.v1";
const MIN_ADVANCEMENT_INTERVAL: Duration = Duration::from_secs(1);

pub(super) struct ModuleProgressProjector {
    bridge: Arc<AcpBridge>,
    started: Instant,
    state: Mutex<ProjectionState>,
}

impl ModuleProgressProjector {
    pub(super) fn start(bridge: Arc<AcpBridge>) -> Arc<Self> {
        let projector = Arc::new(Self {
            bridge,
            started: Instant::now(),
            state: Mutex::new(ProjectionState::default()),
        });
        projector.send("started", ModulePhaseStats::default());
        projector
    }

    pub(super) fn advance(&self, stats: ModulePhaseStats) {
        let elapsed = self.started.elapsed();
        let should_emit = self
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .advance(elapsed, stats);
        if should_emit {
            self.send("advancing", stats);
        }
    }

    pub(super) fn finish(&self, stats: ModulePhaseStats) {
        self.send("completed", stats);
    }

    fn send(&self, state: &str, stats: ModulePhaseStats) {
        let progress = stats
            .modules_compiled
            .saturating_add(stats.modules_loaded)
            .min(i64::MAX as u64) as i64;
        self.bridge.send_progress(
            PHASE,
            match state {
                "started" => "Preparing modules",
                "completed" => "Modules prepared",
                _ => "Preparing modules",
            },
            Some(progress),
            None,
            Some(serde_json::json!({
                "schema": SCHEMA,
                "state": state,
                "module_compile_ms": stats.module_compile_ms,
                "module_load_ms": stats.module_load_ms,
                "modules_compiled": stats.modules_compiled,
                "modules_loaded": stats.modules_loaded,
            })),
        );
    }
}

#[derive(Default)]
struct ProjectionState {
    last_emitted_at: Duration,
    last_emitted: ModulePhaseStats,
    emitted_advancement: bool,
}

impl ProjectionState {
    fn advance(&mut self, now: Duration, stats: ModulePhaseStats) -> bool {
        let advanced = stats.modules_compiled > self.last_emitted.modules_compiled
            || stats.modules_loaded > self.last_emitted.modules_loaded;
        if !advanced {
            return false;
        }
        if self.emitted_advancement
            && now.saturating_sub(self.last_emitted_at) < MIN_ADVANCEMENT_INTERVAL
        {
            return false;
        }
        self.last_emitted_at = now;
        self.last_emitted = stats;
        self.emitted_advancement = true;
        true
    }

    #[cfg(test)]
    fn frame_age(&self, now: Duration) -> Duration {
        now.saturating_sub(self.last_emitted_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(compiled: u64, loaded: u64) -> ModulePhaseStats {
        let mut stats = ModulePhaseStats::default();
        stats.modules_compiled = compiled;
        stats.modules_loaded = loaded;
        stats
    }

    #[test]
    fn successful_transitions_advance_monotonically_at_a_time_bound() {
        let mut state = ProjectionState::default();
        assert!(state.advance(Duration::from_millis(10), stats(1, 0)));
        assert!(!state.advance(Duration::from_millis(20), stats(1, 1)));
        assert!(!state.advance(Duration::from_millis(30), stats(1, 1)));
        assert!(state.advance(Duration::from_millis(1_010), stats(1, 2)));
        assert_eq!(state.last_emitted, stats(1, 2));
    }

    #[test]
    fn stuck_preparation_cannot_refresh_the_live_frame() {
        let state = ProjectionState::default();
        assert!(
            state.frame_age(Duration::from_secs(31)) > Duration::from_secs(30),
            "without a successful recorder transition the start frame must age out"
        );
    }
}
