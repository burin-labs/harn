use std::fs;
use std::path::PathBuf;
use std::process;

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

/// Write JUnit XML report.
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

pub(crate) async fn run_conformance_tests(
    dir: &str,
    filter: Option<&str>,
    junit_path: Option<&str>,
    timeout_ms: u64,
    verbose: bool,
) {
    let dir_path = PathBuf::from(dir);
    if !dir_path.exists() {
        eprintln!("Directory not found: {dir}");
        process::exit(1);
    }

    let suite_start = std::time::Instant::now();

    let mut passed = 0;
    let mut failed = 0;
    let mut errors: Vec<String> = Vec::new();
    // (name, passed, error_msg, duration_ms)
    let mut junit_results: Vec<(String, bool, String, u64)> = Vec::new();

    let mut test_dirs = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir_path) {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                test_dirs.push(entry.path());
            }
        }
    }
    test_dirs.sort();
    test_dirs.insert(0, dir_path.clone());

    for test_dir in &test_dirs {
        let mut harn_files: Vec<PathBuf> = Vec::new();
        if let Ok(entries) = fs::read_dir(test_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map(|e| e == "harn").unwrap_or(false) {
                    harn_files.push(path);
                }
            }
        }
        harn_files.sort();

        for harn_file in &harn_files {
            let expected_file = harn_file.with_extension("expected");
            let error_file = harn_file.with_extension("error");

            let rel_path = harn_file
                .strip_prefix(&dir_path)
                .unwrap_or(harn_file)
                .display()
                .to_string();

            // Apply filter
            if let Some(pattern) = filter {
                if !rel_path.contains(pattern) {
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

                // Reset thread-local state between conformance tests
                harn_vm::reset_thread_local_state();

                let start = std::time::Instant::now();
                let result = tokio::time::timeout(
                    std::time::Duration::from_millis(timeout_ms),
                    execute(&source, Some(harn_file.as_path())),
                )
                .await;
                let duration_ms = start.elapsed().as_millis() as u64;

                match result {
                    Ok(Ok(output)) => {
                        let actual = normalize_actual_output(output.trim_end());
                        if actual == expected {
                            if verbose {
                                println!("  \x1b[32mPASS\x1b[0m  {rel_path} ({duration_ms} ms)");
                            } else {
                                println!("  \x1b[32mPASS\x1b[0m  {rel_path}");
                            }
                            junit_results.push((rel_path, true, String::new(), duration_ms));
                            passed += 1;
                        } else {
                            if verbose {
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

                // Reset thread-local state between conformance tests
                harn_vm::reset_thread_local_state();

                let start = std::time::Instant::now();
                let result = tokio::time::timeout(
                    std::time::Duration::from_millis(timeout_ms),
                    execute(&source, Some(harn_file.as_path())),
                )
                .await;
                let duration_ms = start.elapsed().as_millis() as u64;

                match result {
                    Ok(Err(ref err)) if err.contains(&expected_error) => {
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

    // Verbose timing summary
    if verbose {
        println!();
        println!("Total time: {total_duration_ms} ms");

        // Show slowest 5 tests
        let mut by_time: Vec<&(String, bool, String, u64)> = junit_results.iter().collect();
        by_time.sort_by(|a, b| b.3.cmp(&a.3));
        let top_n = by_time.len().min(5);
        if top_n > 0 {
            println!();
            println!("Slowest {top_n} tests:");
            for entry in &by_time[..top_n] {
                println!("  {} ms  {}", entry.3, entry.0);
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
    // Count unique files
    let file_count = summary
        .results
        .iter()
        .map(|r| r.file.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len();

    // Test count header
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
                // Indent multi-line errors
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

    // Initial run
    let summary = test_runner::run_tests(&path, filter, timeout_ms, parallel).await;
    print_test_results(&summary);

    // Set up file watcher
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
        // Wait for a file change event
        match rx.recv() {
            Ok(Ok(event)) => {
                // Only re-run for .harn file modifications
                let is_harn = event
                    .paths
                    .iter()
                    .any(|p| p.extension().is_some_and(|e| e == "harn"));
                if !is_harn {
                    continue;
                }

                // Debounce: drain any queued events
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
