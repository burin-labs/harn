use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use regex::Regex;

use crate::commands::run::{install_cli_llm_mock_mode, CliLlmMockMode};
use crate::env_guard::ScopedEnvVar;
use crate::execute;
use crate::test_runner;

fn normalize_expected_output(text: &str) -> String {
    text.lines()
        .map(normalize_output_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_actual_output(text: &str) -> String {
    text.lines()
        .map(normalize_output_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_output_line(line: &str) -> String {
    if let Some(prefix) = line.strip_suffix("ms") {
        if let Some((head, _millis)) = prefix.rsplit_once(": ") {
            if head.starts_with("[timer] ") {
                return format!("{head}: <ms>");
            }
        }
    }
    line.to_string()
}

/// Produce a simple line diff between expected and actual.
fn simple_diff(expected: &str, actual: &str) -> String {
    let mut result = String::new();
    let expected_lines: Vec<&str> = expected.lines().collect();
    let actual_lines: Vec<&str> = actual.lines().collect();
    let max = expected_lines.len().max(actual_lines.len());
    for i in 0..max {
        let exp = expected_lines.get(i).copied().unwrap_or("");
        let act = actual_lines.get(i).copied().unwrap_or("");
        if exp == act {
            result.push_str(&format!("  {exp}\n"));
        } else {
            result.push_str(&format!("\x1b[31m- {exp}\x1b[0m\n"));
            result.push_str(&format!("\x1b[32m+ {act}\x1b[0m\n"));
        }
    }
    result
}

/// Check whether an actual error message matches the expected error spec.
///
/// The `.error` file supports three modes:
/// - Plain text: substring match (backward compatible)
/// - `re:` prefix: regex match against the full error message
/// - Multiple lines: union — passes if any line matches
fn error_matches(actual_error: &str, expected_spec: &str) -> bool {
    let lines: Vec<&str> = expected_spec.lines().collect();
    if lines.len() > 1 {
        return lines
            .iter()
            .any(|line| error_line_matches(actual_error, line.trim()));
    }
    error_line_matches(actual_error, expected_spec.trim())
}

fn error_line_matches(actual_error: &str, pattern: &str) -> bool {
    if let Some(re_pattern) = pattern.strip_prefix("re:") {
        match Regex::new(re_pattern.trim()) {
            Ok(re) => re.is_match(actual_error),
            Err(_) => {
                eprintln!("    warning: invalid regex in .error file: {re_pattern}");
                false
            }
        }
    } else {
        actual_error.contains(pattern)
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn write_junit_xml(path: &str, results: &[(String, bool, String, u64)]) {
    let total = results.len();
    let failures = results.iter().filter(|r| !r.1).count();
    let total_time: f64 = results.iter().map(|r| r.3 as f64 / 1000.0).sum();

    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(&format!(
        "<testsuite name=\"harn\" tests=\"{total}\" failures=\"{failures}\" time=\"{total_time:.3}\">\n"
    ));
    for (name, passed, error_msg, duration_ms) in results {
        let time = *duration_ms as f64 / 1000.0;
        let escaped_name = xml_escape(name);
        xml.push_str(&format!(
            "  <testcase name=\"{escaped_name}\" time=\"{time:.3}\""
        ));
        if *passed {
            xml.push_str(" />\n");
        } else {
            xml.push_str(">\n");
            let escaped = xml_escape(error_msg);
            xml.push_str(&format!(
                "    <failure message=\"test failed\">{escaped}</failure>\n"
            ));
            xml.push_str("  </testcase>\n");
        }
    }
    xml.push_str("</testsuite>\n");

    if let Err(e) = fs::write(path, &xml) {
        eprintln!("Failed to write JUnit XML to {path}: {e}");
    } else {
        println!("JUnit XML written to {path}");
    }
}

fn collect_harn_files_sorted(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    super::collect_harn_files(dir, &mut files);
    files
}

fn conformance_llm_mock_mode(harn_file: &Path) -> CliLlmMockMode {
    let fixture = harn_file.with_extension("llm-mock.jsonl");
    if fixture.is_file() {
        CliLlmMockMode::Replay {
            fixture_path: fixture,
        }
    } else {
        CliLlmMockMode::Off
    }
}

fn canonicalize_or_err(path: &Path) -> Result<PathBuf, String> {
    path.canonicalize()
        .map_err(|error| format!("Failed to canonicalize {}: {error}", path.display()))
}

fn resolve_conformance_selection(
    suite_root: &Path,
    selection: Option<&str>,
) -> Result<Vec<PathBuf>, String> {
    let suite_root = canonicalize_or_err(suite_root)?;

    let Some(selection) = selection else {
        return Ok(collect_harn_files_sorted(&suite_root));
    };

    let raw = PathBuf::from(selection);
    let mut candidates = vec![raw.clone()];
    if !raw.is_absolute() && !raw.starts_with(&suite_root) {
        candidates.push(suite_root.join(&raw));
    }

    let Some(candidate) = candidates.into_iter().find(|path| path.exists()) else {
        return Err(format!(
            "Conformance target not found: {selection}. Expected a file or directory under {}",
            suite_root.display()
        ));
    };

    let canonical = canonicalize_or_err(&candidate)?;
    if !canonical.starts_with(&suite_root) {
        return Err(format!(
            "Conformance target must be inside {}: {}",
            suite_root.display(),
            candidate.display()
        ));
    }

    if canonical.is_file() {
        if canonical.extension().is_some_and(|ext| ext == "harn") {
            return Ok(vec![canonical]);
        }
        return Err(format!(
            "Conformance target must be a .harn file or directory: {}",
            candidate.display()
        ));
    }

    let files = collect_harn_files_sorted(&canonical);
    if files.is_empty() {
        return Err(format!(
            "No .harn conformance tests found under {}",
            candidate.display()
        ));
    }
    Ok(files)
}

pub(crate) async fn run_conformance_tests(
    dir: &str,
    selection: Option<&str>,
    filter: Option<&str>,
    junit_path: Option<&str>,
    timeout_ms: u64,
    verbose: bool,
    timing: bool,
) {
    let show_timing = verbose || timing;
    let _disable_llm_calls = ScopedEnvVar::set(harn_vm::llm::LLM_CALLS_DISABLED_ENV, "1");
    let dir_path = PathBuf::from(dir);
    if !dir_path.exists() {
        eprintln!("Directory not found: {dir}");
        process::exit(1);
    }
    let suite_root = canonicalize_or_err(&dir_path).unwrap_or_else(|error| {
        eprintln!("{error}");
        process::exit(1);
    });

    let suite_start = std::time::Instant::now();

    let mut passed = 0;
    let mut failed = 0;
    let mut errors: Vec<String> = Vec::new();
    let mut junit_results: Vec<(String, bool, String, u64)> = Vec::new();

    let harn_files =
        resolve_conformance_selection(&suite_root, selection).unwrap_or_else(|error| {
            eprintln!("{error}");
            process::exit(1);
        });

    for harn_file in &harn_files {
        let expected_file = harn_file.with_extension("expected");
        let error_file = harn_file.with_extension("error");

        let rel_path = harn_file
            .strip_prefix(&suite_root)
            .unwrap_or(harn_file)
            .display()
            .to_string();

        // Filter syntax: `re:<regex>`, `foo|bar` (OR), `*_runtime*` (glob),
        // or plain substring match.
        if let Some(pattern) = filter {
            let matched = if let Some(re_pat) = pattern.strip_prefix("re:") {
                Regex::new(re_pat).is_ok_and(|re| re.is_match(&rel_path))
            } else if pattern.contains('|') {
                pattern.split('|').any(|p| rel_path.contains(p.trim()))
            } else if pattern.contains('*') || pattern.contains('?') {
                let escaped = regex::escape(pattern)
                    .replace(r"\*", ".*")
                    .replace(r"\?", ".");
                Regex::new(&escaped).is_ok_and(|re| re.is_match(&rel_path))
            } else {
                rel_path.contains(pattern)
            };
            if !matched {
                continue;
            }
        }

        if expected_file.exists() {
            let source = match fs::read_to_string(harn_file) {
                Ok(s) => s,
                Err(e) => {
                    println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                    let msg = format!("{rel_path}: IO error reading source: {e}");
                    errors.push(msg.clone());
                    junit_results.push((rel_path, false, msg, 0));
                    failed += 1;
                    continue;
                }
            };
            let expected = match fs::read_to_string(&expected_file) {
                Ok(s) => normalize_expected_output(s.trim_end()),
                Err(e) => {
                    println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                    let msg = format!("{rel_path}: IO error reading expected: {e}");
                    errors.push(msg.clone());
                    junit_results.push((rel_path, false, msg, 0));
                    failed += 1;
                    continue;
                }
            };

            harn_vm::reset_thread_local_state();
            let llm_mock_mode = conformance_llm_mock_mode(harn_file);
            if let Err(error) = install_cli_llm_mock_mode(&llm_mock_mode) {
                println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                let msg = format!("{rel_path}: llm mock setup error: {error}");
                errors.push(msg.clone());
                junit_results.push((rel_path, false, msg, 0));
                failed += 1;
                continue;
            }

            let start = std::time::Instant::now();
            let result = tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms),
                execute(&source, Some(harn_file.as_path())),
            )
            .await;
            let duration_ms = start.elapsed().as_millis() as u64;
            harn_vm::llm::clear_cli_llm_mock_mode();

            match result {
                Ok(Ok(output)) => {
                    let actual = normalize_actual_output(output.trim_end());
                    if actual == expected {
                        if show_timing {
                            println!("  \x1b[32mPASS\x1b[0m  {rel_path} ({duration_ms} ms)");
                        } else {
                            println!("  \x1b[32mPASS\x1b[0m  {rel_path}");
                        }
                        junit_results.push((rel_path, true, String::new(), duration_ms));
                        passed += 1;
                    } else {
                        if show_timing {
                            println!("  \x1b[31mFAIL\x1b[0m  {rel_path} ({duration_ms} ms)");
                        } else {
                            println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                        }
                        let diff = simple_diff(&expected, &actual);
                        let msg = if verbose {
                            format!(
                                "{rel_path}:\n  expected:\n    {}\n  actual:\n    {}\n  diff:\n{diff}",
                                expected.lines().collect::<Vec<_>>().join("\n    "),
                                actual.lines().collect::<Vec<_>>().join("\n    "),
                            )
                        } else {
                            format!("{rel_path}:\n{diff}")
                        };
                        errors.push(msg.clone());
                        junit_results.push((rel_path, false, msg, duration_ms));
                        failed += 1;
                    }
                }
                Ok(Err(e)) => {
                    if verbose {
                        println!("  \x1b[31mFAIL\x1b[0m  {rel_path} ({duration_ms} ms)");
                    } else {
                        println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                    }
                    let msg = format!("{rel_path}: runtime error: {e}");
                    errors.push(msg.clone());
                    junit_results.push((rel_path, false, msg, duration_ms));
                    failed += 1;
                }
                Err(_) => {
                    println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                    let msg = format!("{rel_path}: timed out after {timeout_ms}ms");
                    errors.push(msg.clone());
                    junit_results.push((rel_path, false, msg, timeout_ms));
                    failed += 1;
                }
            }
        } else if error_file.exists() {
            let source = match fs::read_to_string(harn_file) {
                Ok(s) => s,
                Err(e) => {
                    println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                    let msg = format!("{rel_path}: IO error reading source: {e}");
                    errors.push(msg.clone());
                    junit_results.push((rel_path, false, msg, 0));
                    failed += 1;
                    continue;
                }
            };
            let expected_error = match fs::read_to_string(&error_file) {
                Ok(s) => s.trim_end().to_string(),
                Err(e) => {
                    println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                    let msg = format!("{rel_path}: IO error reading expected error: {e}");
                    errors.push(msg.clone());
                    junit_results.push((rel_path, false, msg, 0));
                    failed += 1;
                    continue;
                }
            };

            harn_vm::reset_thread_local_state();
            let llm_mock_mode = conformance_llm_mock_mode(harn_file);
            if let Err(error) = install_cli_llm_mock_mode(&llm_mock_mode) {
                println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                let msg = format!("{rel_path}: llm mock setup error: {error}");
                errors.push(msg.clone());
                junit_results.push((rel_path, false, msg, 0));
                failed += 1;
                continue;
            }

            let start = std::time::Instant::now();
            let result = tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms),
                execute(&source, Some(harn_file.as_path())),
            )
            .await;
            let duration_ms = start.elapsed().as_millis() as u64;
            harn_vm::llm::clear_cli_llm_mock_mode();

            match result {
                Ok(Err(ref err)) if error_matches(err, &expected_error) => {
                    if verbose {
                        println!("  \x1b[32mPASS\x1b[0m  {rel_path} ({duration_ms} ms)");
                    } else {
                        println!("  \x1b[32mPASS\x1b[0m  {rel_path}");
                    }
                    junit_results.push((rel_path, true, String::new(), duration_ms));
                    passed += 1;
                }
                Ok(Err(err)) => {
                    if verbose {
                        println!("  \x1b[31mFAIL\x1b[0m  {rel_path} ({duration_ms} ms)");
                    } else {
                        println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                    }
                    let msg = format!(
                        "{rel_path}:\n  expected error containing: {expected_error}\n  actual error: {err}"
                    );
                    errors.push(msg.clone());
                    junit_results.push((rel_path, false, msg, duration_ms));
                    failed += 1;
                }
                Ok(Ok(_)) => {
                    println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                    let msg = format!(
                        "{rel_path}: expected error containing '{expected_error}', but succeeded"
                    );
                    errors.push(msg.clone());
                    junit_results.push((rel_path, false, msg, duration_ms));
                    failed += 1;
                }
                Err(_) => {
                    println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                    let msg = format!("{rel_path}: timed out after {timeout_ms}ms");
                    errors.push(msg.clone());
                    junit_results.push((rel_path, false, msg, timeout_ms));
                    failed += 1;
                }
            }
        }
    }

    let total_duration_ms = suite_start.elapsed().as_millis() as u64;

    println!();
    if failed > 0 {
        println!(
            "\x1b[31m{passed} passed, {failed} failed, {} total\x1b[0m",
            passed + failed
        );
    } else {
        println!(
            "\x1b[32m{passed} passed, {failed} failed, {} total\x1b[0m",
            passed + failed
        );
    }

    if show_timing {
        println!();
        println!("Total time: {total_duration_ms} ms");

        let mut durations: Vec<u64> = junit_results.iter().map(|r| r.3).collect();
        durations.sort();

        if !durations.is_empty() {
            let n = durations.len();
            let p50 = durations[n * 50 / 100];
            let p95 = durations[n * 95 / 100];
            let p99 = durations[(n * 99 / 100).min(n - 1)];
            let avg = durations.iter().sum::<u64>() / n as u64;
            println!("Per-test: avg={avg} ms  p50={p50} ms  p95={p95} ms  p99={p99} ms");
        }

        let mut by_time: Vec<&(String, bool, String, u64)> = junit_results.iter().collect();
        by_time.sort_by_key(|entry| std::cmp::Reverse(entry.3));
        let top_n = by_time.len().min(10);
        if top_n > 0 {
            println!();
            println!("Slowest {top_n} tests:");
            for entry in &by_time[..top_n] {
                println!("  {:>6} ms  {}", entry.3, entry.0);
            }
        }
    }

    if let Some(path) = junit_path {
        write_junit_xml(path, &junit_results);
    }

    if !errors.is_empty() {
        println!();
        println!("Failures:");
        for err in &errors {
            println!("  {err}");
        }
        process::exit(1);
    }
}

fn print_test_results(summary: &test_runner::TestSummary) {
    let file_count = summary
        .results
        .iter()
        .map(|r| r.file.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len();

    if summary.total > 0 {
        println!(
            "Running {} test{} from {} file{}...\n",
            summary.total,
            if summary.total == 1 { "" } else { "s" },
            file_count,
            if file_count == 1 { "" } else { "s" },
        );
    }

    for result in &summary.results {
        if result.passed {
            println!(
                "  \x1b[32mPASS\x1b[0m  {} [{}] ({} ms)",
                result.name, result.file, result.duration_ms
            );
        } else {
            println!("  \x1b[31mFAIL\x1b[0m  {} [{}]", result.name, result.file);
            if let Some(err) = &result.error {
                for line in err.lines() {
                    println!("        {line}");
                }
            }
        }
    }

    println!();
    if summary.failed > 0 {
        println!(
            "\x1b[31m{} passed, {} failed, {} total ({} ms)\x1b[0m",
            summary.passed, summary.failed, summary.total, summary.duration_ms
        );
    } else if summary.total == 0 {
        println!("No test pipelines found");
    } else {
        println!(
            "\x1b[32m{} passed, {} total ({} ms)\x1b[0m",
            summary.passed, summary.total, summary.duration_ms
        );
    }
}

pub(crate) async fn run_user_tests(
    path_str: &str,
    filter: Option<&str>,
    timeout_ms: u64,
    parallel: bool,
) {
    let path = PathBuf::from(path_str);
    if !path.exists() {
        eprintln!("Path not found: {path_str}");
        process::exit(1);
    }
    let summary = test_runner::run_tests(&path, filter, timeout_ms, parallel).await;
    print_test_results(&summary);
    if summary.failed > 0 {
        process::exit(1);
    }
}

pub(crate) async fn run_watch_tests(
    path_str: &str,
    filter: Option<&str>,
    timeout_ms: u64,
    parallel: bool,
) {
    use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::mpsc;
    use std::time::Duration;

    let path = PathBuf::from(path_str);
    if !path.exists() {
        eprintln!("Path not found: {path_str}");
        process::exit(1);
    }

    println!("Watching {path_str} for changes... (Ctrl+C to stop)\n");

    let summary = test_runner::run_tests(&path, filter, timeout_ms, parallel).await;
    print_test_results(&summary);

    let (tx, rx) = mpsc::channel();
    let mut watcher = RecommendedWatcher::new(tx, Config::default()).unwrap_or_else(|e| {
        eprintln!("Failed to create file watcher: {e}");
        process::exit(1);
    });
    watcher
        .watch(&path, RecursiveMode::Recursive)
        .unwrap_or_else(|e| {
            eprintln!("Failed to watch {path_str}: {e}");
            process::exit(1);
        });

    loop {
        match rx.recv() {
            Ok(Ok(event)) => {
                let is_harn = event
                    .paths
                    .iter()
                    .any(|p| p.extension().is_some_and(|e| e == "harn"));
                if !is_harn {
                    continue;
                }

                // Debounce: drain any additional events within 100ms.
                while rx.recv_timeout(Duration::from_millis(100)).is_ok() {}

                println!("\n\x1b[2m--- file changed, re-running tests ---\x1b[0m\n");
                let summary = test_runner::run_tests(&path, filter, timeout_ms, parallel).await;
                print_test_results(&summary);
            }
            Ok(Err(e)) => {
                eprintln!("Watch error: {e}");
            }
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{collect_harn_files_sorted, resolve_conformance_selection};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempTestDir {
        path: PathBuf,
    }

    static TEMP_DIR_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    impl TempTestDir {
        fn new() -> Self {
            let unique = format!(
                "harn-cli-test-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                TEMP_DIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            );
            let path = std::env::temp_dir().join(unique);
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn write(&self, relative: &str) {
            let path = self.path.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, "// test").unwrap();
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempTestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn collect_harn_files_sorted_descends_and_sorts() {
        let temp = TempTestDir::new();
        temp.write("suite/zeta.harn");
        temp.write("suite/alpha.harn");
        temp.write("suite/nested/beta.harn");
        fs::write(temp.path().join("suite/ignore.txt"), "").unwrap();

        let files = collect_harn_files_sorted(&temp.path().join("suite"));
        let relative: Vec<String> = files
            .iter()
            .map(|path| {
                path.strip_prefix(temp.path())
                    .unwrap()
                    .display()
                    .to_string()
            })
            .collect();

        assert_eq!(
            relative,
            vec![
                "suite/alpha.harn",
                "suite/nested/beta.harn",
                "suite/zeta.harn"
            ]
        );
    }

    #[test]
    fn resolve_conformance_selection_accepts_suite_relative_file() {
        let temp = TempTestDir::new();
        temp.write("conformance/tests/sample.harn");

        let files = resolve_conformance_selection(
            &temp.path().join("conformance"),
            Some("tests/sample.harn"),
        )
        .unwrap();

        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("conformance/tests/sample.harn"));
    }

    #[test]
    fn resolve_conformance_selection_rejects_paths_outside_suite_root() {
        let temp = TempTestDir::new();
        temp.write("conformance/tests/sample.harn");
        temp.write("outside.harn");

        let error = resolve_conformance_selection(
            &temp.path().join("conformance"),
            Some("../outside.harn"),
        )
        .unwrap_err();

        assert!(error.contains("must be inside"));
    }
}
