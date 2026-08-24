use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crate::{FixtureScope, SuiteCallablePreparation, TestCase, TestResult, TestRunSession};

#[doc(hidden)]
pub struct PreparedCallableCases {
    pub cases: Vec<TestCase>,
    pub failures: Vec<TestResult>,
    pub timing: SuiteCallablePreparation,
}

/// Compile every selected callable in a source file in one compiler pass and
/// project the immutable entries onto fresh-isolate test cases.
#[doc(hidden)]
pub fn prepare_callable_entries(
    mut cases: Vec<TestCase>,
    session: &TestRunSession,
) -> PreparedCallableCases {
    let started = Instant::now();
    let mut by_file: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
    for (index, case) in cases.iter().enumerate() {
        by_file.entry(case.file.clone()).or_default().push(index);
    }

    let mut failed = HashSet::new();
    let mut failures = Vec::new();
    let mut compiled_entries = 0usize;
    for indices in by_file.values() {
        let first = &cases[indices[0]];
        let mut request_indices: BTreeMap<(String, Option<String>), Vec<usize>> = BTreeMap::new();
        for &index in indices {
            let case = &cases[index];
            let fixture = case
                .fixture
                .as_ref()
                .filter(|fixture| fixture.scope == FixtureScope::Case)
                .map(|fixture| fixture.name.clone());
            request_indices
                .entry((case.pipeline_name.clone(), fixture))
                .or_default()
                .push(index);
        }
        let owned_requests = request_indices.keys().cloned().collect::<Vec<_>>();
        let requests = owned_requests
            .iter()
            .map(|(pipeline, fixture)| (pipeline.as_str(), fixture.as_deref()))
            .collect::<Vec<_>>();
        let mut fixture_indices: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for &index in indices {
            if let Some(fixture) = cases[index]
                .fixture
                .as_ref()
                .filter(|fixture| fixture.scope == FixtureScope::File)
            {
                fixture_indices
                    .entry(fixture.name.clone())
                    .or_default()
                    .push(index);
            }
        }
        let fixture_names = fixture_indices
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let imported_enums = first.imported_enum_candidates.iter().cloned();
        let compiler = if first.trusted_host_dispatch {
            harn_vm::Compiler::new_trusted_host_dispatch()
                .with_imported_enum_candidates(imported_enums)
        } else {
            harn_vm::Compiler::new().with_imported_enum_candidates(imported_enums)
        };
        match compiler.compile_named_callable_entries(&first.program, &requests, &fixture_names) {
            Ok(batch) => {
                compiled_entries += batch.pipelines.iter().filter(|entry| entry.is_ok()).count();
                for ((_, case_indices), entry) in request_indices.into_iter().zip(batch.pipelines) {
                    match entry {
                        Ok(entry) => {
                            let entry = Arc::new(entry);
                            for index in case_indices {
                                cases[index].compiled_entry = Some(Arc::clone(&entry));
                            }
                        }
                        Err(error) => {
                            for index in case_indices {
                                failed.insert(index);
                                failures.push(compile_failure(&cases[index], error.clone()));
                            }
                        }
                    }
                }
                compiled_entries += batch.functions.iter().filter(|entry| entry.is_ok()).count();
                for ((_, case_indices), entry) in fixture_indices.into_iter().zip(batch.functions) {
                    let entry = entry.map(Arc::new);
                    for index in case_indices {
                        cases[index].compiled_file_fixture_entry = Some(entry.clone());
                    }
                }
            }
            Err(error) => {
                for &index in indices {
                    failed.insert(index);
                    failures.push(compile_failure(&cases[index], error.clone()));
                }
            }
        }
    }

    let files = by_file.len();
    cases = cases
        .into_iter()
        .enumerate()
        .filter_map(|(index, case)| (!failed.contains(&index)).then_some(case))
        .collect();
    session.record_callable_preparation(files, compiled_entries);
    PreparedCallableCases {
        cases,
        failures,
        timing: SuiteCallablePreparation {
            duration_ms: started.elapsed().as_millis() as u64,
            files,
            entries: compiled_entries,
        },
    }
}

fn compile_failure(case: &TestCase, error: harn_vm::CompileError) -> TestResult {
    TestResult {
        name: case.name.clone(),
        file: case.file.display().to_string(),
        passed: false,
        error: Some(format!("Compile error: {error}")),
        captured_output: None,
        timeout: None,
        duration_ms: 0,
        phases: None,
        timing_spans: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use super::prepare_callable_entries;
    use crate::{extract_cases_from_program, parse_program, TestRunSession};

    #[test]
    fn selected_file_compiles_all_callable_entries_once() {
        let source = Arc::new(
            "pipeline test_one(task: unknown) { assert(true) }\n\
             pipeline test_two(task: unknown) { assert(true) }\n"
                .to_string(),
        );
        let program = Arc::new(parse_program(&source).unwrap());
        let cases = extract_cases_from_program(
            Path::new("test_compile_once.harn"),
            &source,
            &program,
            None,
            usize::MAX,
        )
        .unwrap();
        let session = TestRunSession::default();

        let prepared = prepare_callable_entries(cases, &session);

        assert!(prepared.failures.is_empty());
        assert_eq!(prepared.cases.len(), 2);
        assert_eq!(prepared.timing.files, 1);
        assert_eq!(prepared.timing.entries, 2);
        assert!(prepared
            .cases
            .iter()
            .all(|case| case.compiled_entry.is_some()));
        let stats = session.stats();
        assert_eq!(stats.test_files_compiled, 1);
        assert_eq!(stats.test_entries_compiled, 2);
    }
}
