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
    let scheduler = Arc::new(CaseScheduler::new(cases, workers));
    let results: Arc<Mutex<Vec<TestResult>>> = Arc::new(Mutex::new(Vec::new()));
    let infrastructure_errors: Arc<Mutex<Vec<TestResult>>> = Arc::new(Mutex::new(Vec::new()));
    let cancelled = Arc::new(AtomicBool::new(false));
    let completed = Arc::new(Mutex::new(0usize));
    let init_worker = Arc::new(init_worker);
    let execute = Arc::new(execute);

    let mut handles = Vec::with_capacity(workers);
    for worker_idx in 0..workers {
        let scheduler = Arc::clone(&scheduler);
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
                    let Some(claim) = scheduler.claim(&cancelled, fail_fast) else {
                        break;
                    };
                    if fail_fast && cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    let case = claim.case();
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
                    let result = execute(&mut worker, case);
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

fn next_test_index(counter: &Mutex<usize>) -> usize {
    let mut guard = counter.lock().unwrap();
    *guard += 1;
    *guard
}

struct CaseScheduler {
    state: Mutex<SchedulerState>,
    cond: Condvar,
    capacity: usize,
}

struct SchedulerState {
    queue: Vec<TestCase>,
    available: usize,
    busy_groups: HashSet<String>,
}

struct CaseClaim {
    scheduler: Arc<CaseScheduler>,
    case: TestCase,
    weight: usize,
    group: Option<String>,
}

impl CaseScheduler {
    fn new(queue: Vec<TestCase>, capacity: usize) -> Self {
        Self {
            state: Mutex::new(SchedulerState {
                queue,
                available: capacity,
                busy_groups: HashSet::new(),
            }),
            cond: Condvar::new(),
            capacity,
        }
    }

    fn claim(self: &Arc<Self>, cancelled: &AtomicBool, fail_fast: bool) -> Option<CaseClaim> {
        let mut state = self.state.lock().unwrap();
        loop {
            if fail_fast && cancelled.load(Ordering::Acquire) {
                return None;
            }
            if state.queue.is_empty() {
                return None;
            }
            if let Some(index) = state.queue.iter().rposition(|case| {
                let weight = case.weight.min(self.capacity).max(1);
                let group_free = case
                    .serial_group
                    .as_deref()
                    .is_none_or(|name| !state.busy_groups.contains(name));
                state.available >= weight && group_free
            }) {
                let case = state.queue.remove(index);
                let weight = case.weight.min(self.capacity).max(1);
                let group = case.serial_group.clone();
                state.available -= weight;
                if let Some(name) = group.as_deref() {
                    state.busy_groups.insert(name.to_string());
                }
                return Some(CaseClaim {
                    scheduler: Arc::clone(self),
                    case,
                    weight,
                    group,
                });
            }
            state = self.cond.wait(state).unwrap();
        }
    }
}

impl CaseClaim {
    fn case(&self) -> &TestCase {
        &self.case
    }
}

impl Drop for CaseClaim {
    fn drop(&mut self) {
        let mut state = self.scheduler.state.lock().unwrap();
        state.available += self.weight;
        if let Some(group) = self.group.as_deref() {
            state.busy_groups.remove(group);
        }
        self.scheduler.cond.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    use super::{execute_parallel_cases, CaseScheduler, ParallelRunOptions, TestRunEvent};
    use crate::{extract_cases_from_program, parse_program, TestResult};

    #[test]
    fn scheduler_skips_blocked_cases_without_losing_priority() {
        let source = Arc::new(
            "@test\npipeline test_light(task) {}\n\
             @test\n@heavy(threads: 3)\npipeline test_heavy_waiting(task) {}\n\
             @test\n@heavy(threads: 2)\npipeline test_heavy_active(task) {}\n"
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
        let scheduler = Arc::new(CaseScheduler::new(cases, 3));
        let cancelled = AtomicBool::new(false);

        let active = scheduler
            .claim(&cancelled, false)
            .expect("the highest-priority heavy case starts");
        assert_eq!(active.case().name, "test_heavy_active");
        let light = scheduler
            .claim(&cancelled, false)
            .expect("runnable light work bypasses the blocked heavy case");
        assert_eq!(light.case().name, "test_light");
        drop(light);
        drop(active);

        let waiting = scheduler
            .claim(&cancelled, false)
            .expect("the blocked heavy case keeps its place");
        assert_eq!(waiting.case().name, "test_heavy_waiting");
    }

    #[test]
    fn scheduler_skips_a_busy_serial_group() {
        let source = Arc::new(
            "@test\npipeline test_light(task) {}\n\
             @test\n@serial(group: \"fixture\")\npipeline test_serial_waiting(task) {}\n\
             @test\n@serial(group: \"fixture\")\npipeline test_serial_active(task) {}\n"
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
        let scheduler = Arc::new(CaseScheduler::new(cases, 2));
        let cancelled = AtomicBool::new(false);

        let active = scheduler.claim(&cancelled, false).unwrap();
        assert_eq!(active.case().name, "test_serial_active");
        let light = scheduler.claim(&cancelled, false).unwrap();
        assert_eq!(light.case().name, "test_light");
        drop(light);
        drop(active);

        let waiting = scheduler.claim(&cancelled, false).unwrap();
        assert_eq!(waiting.case().name, "test_serial_waiting");
    }

    #[test]
    fn fail_fast_stops_claiming_and_progress_is_event_driven() {
        let source = Arc::new(
            "pipeline test_one(task) { assert(true) }\n\
             pipeline test_two(task) { assert(true) }\n"
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
