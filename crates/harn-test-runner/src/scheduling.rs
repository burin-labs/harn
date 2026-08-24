use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crate::{TestCase, TestResult};

#[derive(Clone, Debug)]
pub enum TestRunEvent {
    SuiteDiscovered {
        total_tests: usize,
        total_files: usize,
        parallel: bool,
        workers: usize,
    },
    LargeSequentialSuite {
        total_tests: usize,
        total_files: usize,
    },
    TestStarted {
        name: String,
        file: String,
        test_index: usize,
        total_tests: usize,
    },
    TestFinished(TestResult),
}

pub type TestRunProgress = Arc<dyn Fn(TestRunEvent) + Send + Sync>;

#[doc(hidden)]
pub struct ParallelCaseResults {
    pub cases: Vec<TestResult>,
    pub infrastructure_errors: Vec<TestResult>,
}

#[doc(hidden)]
pub struct ParallelRunOptions {
    pub workers: usize,
    pub total_tests: usize,
    pub stack_size: usize,
    pub fail_fast: bool,
    pub progress: Option<TestRunProgress>,
}

/// Execute already-discovered cases on owned worker threads.
///
/// The engine owns claiming, weighted resource permits, serial groups,
/// fail-fast barriers, progress ordering, and worker lifecycle. The host
/// adapter supplies only worker-local runtime construction and one-case
/// execution, keeping CLI/package capability wiring out of this crate.
#[doc(hidden)]
pub fn execute_parallel_cases<W, Init, Execute>(
    cases: Vec<TestCase>,
    options: ParallelRunOptions,
    init_worker: Init,
    execute: Execute,
) -> ParallelCaseResults
where
    W: Send + 'static,
    Init: Fn(usize) -> Result<W, String> + Send + Sync + 'static,
    Execute: Fn(&mut W, &TestCase) -> TestResult + Send + Sync + 'static,
{
    let ParallelRunOptions {
        workers,
        total_tests,
        stack_size,
        fail_fast,
        progress,
    } = options;
    let queue = Arc::new(Mutex::new(cases));
    let gate = Arc::new(ResourceGate::new(workers));
    let results: Arc<Mutex<Vec<TestResult>>> = Arc::new(Mutex::new(Vec::new()));
    let infrastructure_errors: Arc<Mutex<Vec<TestResult>>> = Arc::new(Mutex::new(Vec::new()));
    let cancelled = Arc::new(AtomicBool::new(false));
    let completed = Arc::new(Mutex::new(0usize));
    let init_worker = Arc::new(init_worker);
    let execute = Arc::new(execute);

    let mut handles = Vec::with_capacity(workers);
    for worker_idx in 0..workers {
        let queue = Arc::clone(&queue);
        let gate = Arc::clone(&gate);
        let results = Arc::clone(&results);
        let infrastructure_errors = Arc::clone(&infrastructure_errors);
        let cancelled = Arc::clone(&cancelled);
        let completed = Arc::clone(&completed);
        let progress = progress.clone();
        let init_worker = Arc::clone(&init_worker);
        let execute = Arc::clone(&execute);
        let handle = thread::Builder::new()
            .name(format!("harn-test-worker-{worker_idx}"))
            .stack_size(stack_size)
            .spawn(move || {
                let mut worker = match init_worker(worker_idx) {
                    Ok(worker) => worker,
                    Err(error) => {
                        infrastructure_errors.lock().unwrap().push(TestResult {
                            name: "<worker error>".to_string(),
                            file: String::new(),
                            passed: false,
                            error: Some(error),
                            captured_output: None,
                            timeout: None,
                            duration_ms: 0,
                            phases: None,
                            timing_spans: Vec::new(),
                        });
                        return;
                    }
                };
                loop {
                    let Some(case) = claim_next_case(&queue, &cancelled, fail_fast) else {
                        break;
                    };
                    let _guard = gate.acquire(case.weight, case.serial_group.as_deref());
                    if fail_fast && cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    let test_index = next_test_index(&completed);
                    emit_progress(
                        &progress,
                        TestRunEvent::TestStarted {
                            name: case.name.clone(),
                            file: case.file.display().to_string(),
                            test_index,
                            total_tests,
                        },
                    );
                    let result = execute(&mut worker, &case);
                    if fail_fast && !result.passed {
                        cancelled.store(true, Ordering::Release);
                    }
                    emit_progress(&progress, TestRunEvent::TestFinished(result.clone()));
                    results.lock().unwrap().push(result);
                }
            })
            .expect("spawning a harn-test worker thread should succeed");
        handles.push(handle);
    }
    for handle in handles {
        let _ = handle.join();
    }

    ParallelCaseResults {
        cases: unwrap_or_clone(results),
        infrastructure_errors: unwrap_or_clone(infrastructure_errors),
    }
}

fn unwrap_or_clone(values: Arc<Mutex<Vec<TestResult>>>) -> Vec<TestResult> {
    Arc::try_unwrap(values)
        .map(|mutex| mutex.into_inner().unwrap_or_default())
        .unwrap_or_else(|arc| arc.lock().unwrap().clone())
}

fn emit_progress(progress: &Option<TestRunProgress>, event: TestRunEvent) {
    if let Some(callback) = progress {
        callback(event);
    }
}

fn claim_next_case(
    queue: &Mutex<Vec<TestCase>>,
    cancelled: &AtomicBool,
    fail_fast: bool,
) -> Option<TestCase> {
    let mut queue = queue.lock().unwrap();
    if fail_fast && cancelled.load(Ordering::Acquire) {
        None
    } else {
        queue.pop()
    }
}

fn next_test_index(counter: &Mutex<usize>) -> usize {
    let mut guard = counter.lock().unwrap();
    *guard += 1;
    *guard
}

struct ResourceGate {
    state: Mutex<GateState>,
    cond: Condvar,
    capacity: usize,
}

struct GateState {
    available: usize,
    busy_groups: HashSet<String>,
}

struct GateGuard<'a> {
    gate: &'a ResourceGate,
    weight: usize,
    group: Option<String>,
}

impl ResourceGate {
    fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(GateState {
                available: capacity,
                busy_groups: HashSet::new(),
            }),
            cond: Condvar::new(),
            capacity,
        }
    }

    fn acquire(&self, weight: usize, group: Option<&str>) -> GateGuard<'_> {
        let weight = weight.min(self.capacity).max(1);
        let mut state = self.state.lock().unwrap();
        loop {
            let group_free = group.is_none_or(|name| !state.busy_groups.contains(name));
            if state.available >= weight && group_free {
                state.available -= weight;
                if let Some(name) = group {
                    state.busy_groups.insert(name.to_string());
                }
                return GateGuard {
                    gate: self,
                    weight,
                    group: group.map(str::to_owned),
                };
            }
            state = self.cond.wait(state).unwrap();
        }
    }

    #[cfg(test)]
    fn try_acquire(&self, weight: usize, group: Option<&str>) -> Option<GateGuard<'_>> {
        let weight = weight.min(self.capacity).max(1);
        let mut state = self.state.lock().unwrap();
        let group_free = group.is_none_or(|name| !state.busy_groups.contains(name));
        if state.available < weight || !group_free {
            return None;
        }
        state.available -= weight;
        if let Some(name) = group {
            state.busy_groups.insert(name.to_string());
        }
        Some(GateGuard {
            gate: self,
            weight,
            group: group.map(str::to_owned),
        })
    }
}

impl Drop for GateGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.gate.state.lock().unwrap();
        state.available += self.weight;
        if let Some(group) = self.group.as_deref() {
            state.busy_groups.remove(group);
        }
        self.gate.cond.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use super::{execute_parallel_cases, ParallelRunOptions, ResourceGate, TestRunEvent};
    use crate::{extract_cases_from_program, parse_program, TestResult};

    #[test]
    fn serial_groups_and_weights_are_structural_not_timing_based() {
        let gate = ResourceGate::new(2);
        let login = gate.acquire(1, Some("login"));
        assert!(gate.try_acquire(1, Some("login")).is_none());
        assert!(gate.try_acquire(1, Some("independent")).is_some());
        drop(login);
        assert!(gate.try_acquire(1, Some("login")).is_some());

        let gate = ResourceGate::new(2);
        let all = gate.acquire(99, None);
        assert!(gate.try_acquire(1, None).is_none());
        drop(all);
        assert!(gate.try_acquire(1, None).is_some());
    }

    #[test]
    fn fail_fast_stops_claiming_and_progress_is_event_driven() {
        let source = Arc::new(
            "pipeline test_one(task: unknown) { assert(true) }\n\
             pipeline test_two(task: unknown) { assert(true) }\n"
                .to_string(),
        );
        let program = Arc::new(parse_program(&source).unwrap());
        let cases = extract_cases_from_program(
            Path::new("test_scheduler.harn"),
            &source,
            &program,
            None,
            usize::MAX,
        )
        .unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&events);
        let progress = Arc::new(move |event| {
            captured.lock().unwrap().push(match event {
                TestRunEvent::TestStarted { .. } => "started",
                TestRunEvent::TestFinished(_) => "finished",
                _ => "suite",
            });
        });

        let run = execute_parallel_cases(
            cases,
            ParallelRunOptions {
                workers: 1,
                total_tests: 2,
                stack_size: 2 * 1024 * 1024,
                fail_fast: true,
                progress: Some(progress),
            },
            |_| Ok(()),
            |_, case| TestResult {
                name: case.name.clone(),
                file: case.file.display().to_string(),
                passed: false,
                error: Some("deterministic failure".to_string()),
                captured_output: None,
                timeout: None,
                duration_ms: 0,
                phases: None,
                timing_spans: Vec::new(),
            },
        );

        assert_eq!(run.cases.len(), 1);
        assert!(run.infrastructure_errors.is_empty());
        assert_eq!(*events.lock().unwrap(), ["started", "finished"]);
    }
}
