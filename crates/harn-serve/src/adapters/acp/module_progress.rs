use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use harn_vm::ModulePhaseStats;

use super::{AcpBridge, AcpOutput};

const PHASE: &str = "module_preparation";
const SCHEMA: &str = "harn.module_preparation.v1";
const MIN_ADVANCEMENT_INTERVAL: Duration = Duration::from_secs(1);

pub(super) struct ModuleProgressProjector {
    bridge: Arc<AcpBridge>,
    started: Instant,
    state: Mutex<ProjectionState>,
    finished: AtomicBool,
    latest: Mutex<ModulePhaseStats>,
}

impl ModuleProgressProjector {
    pub(super) fn start(bridge: Arc<AcpBridge>) -> Arc<Self> {
        let projector = Arc::new(Self {
            bridge,
            started: Instant::now(),
            state: Mutex::new(ProjectionState::default()),
            finished: AtomicBool::new(false),
            latest: Mutex::new(ModulePhaseStats::default()),
        });
        projector.send("started", ModulePhaseStats::default());
        if let Some(gate) = projector.bridge.output.module_progress_gate() {
            gate.attach(projector.clone());
        }
        projector
    }

    pub(super) fn advance(&self, stats: ModulePhaseStats) {
        *self
            .latest
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = stats;
        if self.finished.load(Ordering::Acquire) {
            return;
        }
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

    /// Report preparation complete once. Later child-VM loads and the end of the
    /// turn never relabel a turn that is past preparation.
    pub(super) fn finish(&self, stats: ModulePhaseStats) {
        if !self.finished.swap(true, Ordering::AcqRel) {
            self.send("completed", stats);
        }
    }

    fn finish_with_latest(&self) {
        let stats = *self
            .latest
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.finish(stats);
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

/// Ends module preparation when the pipeline starts doing anything else.
///
/// Imports resolve before the pipeline body runs, so the first frame that is
/// not module progress marks the end of preparation. Waiting for the turn to
/// finish left "Preparing modules" on screen for the whole turn.
#[derive(Clone, Default)]
pub struct ModuleProgressGate {
    projector: Arc<Mutex<Option<Arc<ModuleProgressProjector>>>>,
}

impl ModuleProgressGate {
    pub(super) fn observe(&self, output: AcpOutput) -> AcpOutput {
        output.observed_by(self.clone())
    }

    fn attach(&self, projector: Arc<ModuleProgressProjector>) {
        *self
            .projector
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(projector);
    }

    pub(super) fn observe_frame(&self, line: &str) {
        let mut slot = self
            .projector
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if slot.is_none() || !is_pipeline_activity(line) {
            return;
        }
        let projector = slot.take();
        drop(slot);
        if let Some(projector) = projector {
            projector.finish_with_latest();
        }
    }
}

/// A request or notification other than module progress. Responses don't
/// count: they answer the client and say nothing about the pipeline.
fn is_pipeline_activity(line: &str) -> bool {
    let Ok(frame) = serde_json::from_str::<serde_json::Value>(line) else {
        return false;
    };
    frame.get("method").is_some()
        && frame.pointer("/params/update/_meta/harn/phase") != Some(&serde_json::json!(PHASE))
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
    fn the_first_pipeline_frame_completes_preparation_once_and_ahead_of_itself() {
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = lines.clone();
        let output = AcpOutput::Callback(Arc::new(move |line: &str| {
            sink.lock().unwrap().push(line.to_string());
        }))
        .observed_by(ModuleProgressGate::default());
        let bridge = Arc::new(AcpBridge {
            client_capabilities: Default::default(),
            session_id: "s".to_string(),
            output,
            pending: Default::default(),
            next_id_counter: std::sync::atomic::AtomicU64::new(1),
            cancellation: super::super::SessionCancellation::default().prepare_prompt(),
            script_name: Mutex::new(String::new()),
            assistant_state: Mutex::new(Default::default()),
            host_responder: None,
        });
        let projector = ModuleProgressProjector::start(bridge.clone());
        projector.advance(stats(1, 2));
        let pipeline_frame = r#"{"jsonrpc":"2.0","method":"session/update","params":{}}"#;
        bridge.output.write_line(pipeline_frame);
        bridge.output.write_line(pipeline_frame);
        projector.advance(stats(1, 5));
        projector.finish(stats(1, 5));

        let lines = lines.lock().unwrap().clone();
        let states: Vec<_> = lines
            .iter()
            .map(|line| {
                let frame: serde_json::Value = serde_json::from_str(line).unwrap();
                frame
                    .pointer("/params/update/_meta/harn/data/state")
                    .and_then(|state| state.as_str())
                    .unwrap_or("pipeline")
                    .to_string()
            })
            .collect();
        let completed = states.iter().position(|state| state == "completed");
        assert_eq!(
            states.iter().filter(|state| *state == "completed").count(),
            1,
            "{states:?}"
        );
        assert!(
            completed < states.iter().position(|state| state == "pipeline"),
            "completed must be written ahead of the frame that ended preparation: {states:?}"
        );
        let completed_frame: serde_json::Value =
            serde_json::from_str(&lines[completed.unwrap()]).unwrap();
        assert_eq!(
            completed_frame.pointer("/params/update/_meta/harn/data/modules_loaded"),
            Some(&serde_json::json!(2)),
            "completion carries the counters reached when preparation ended"
        );
    }

    #[test]
    fn only_non_module_requests_and_notifications_end_preparation() {
        assert!(is_pipeline_activity(
            r#"{"jsonrpc":"2.0","method":"session/update","params":{}}"#
        ));
        assert!(is_pipeline_activity(
            r#"{"jsonrpc":"2.0","id":3,"method":"host/call"}"#
        ));
        let module_progress = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "s",
                "update": super::super::bridge::progress_update(
                    PHASE,
                    "Preparing modules",
                    Some(1),
                    None,
                    Some(serde_json::json!({ "schema": SCHEMA })),
                ),
            },
        });
        assert!(!is_pipeline_activity(&module_progress.to_string()));
        assert!(!is_pipeline_activity(
            r#"{"jsonrpc":"2.0","id":3,"result":{}}"#
        ));
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
