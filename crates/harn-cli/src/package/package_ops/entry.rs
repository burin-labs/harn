use super::*;

pub fn check_package(anchor: Option<&Path>, json: bool) {
    match check_package_impl(anchor) {
        Ok(report) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#))
                );
            } else {
                print_package_check_report(&report);
            }
            if !report.errors.is_empty() {
                process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn pack_package(anchor: Option<&Path>, output: Option<&Path>, dry_run: bool, json: bool) {
    match pack_package_impl(anchor, output, dry_run) {
        Ok(report) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#))
                );
            } else {
                print_package_pack_report(&report);
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn generate_package_docs(anchor: Option<&Path>, output: Option<&Path>, check: bool) {
    match generate_package_docs_impl(anchor, output, check) {
        Ok(path) if check => println!("{} is up to date.", path.display()),
        Ok(path) => println!("Wrote {}.", path.display()),
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn publish_package(
    anchor: Option<&Path>,
    dry_run: bool,
    remote: &str,
    index_repo: &str,
    index_path: &Path,
    registry_name: Option<&str>,
    skip_index_pr: bool,
    registry: Option<&str>,
    json: bool,
) {
    let options = PackagePublishOptions {
        dry_run,
        remote,
        index_repo,
        index_path,
        registry_name,
        skip_index_pr,
        registry,
    };

    match publish_package_impl(anchor, &options) {
        Ok(report) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#))
                );
            } else {
                if report.dry_run {
                    println!("Publish dry run to {} succeeded.", report.registry);
                } else {
                    println!("Published {}.", report.tag);
                }
                println!("tag: {}", report.tag);
                println!("sha: {}", report.sha);
                if let Some(command) = report.tag_command.as_deref() {
                    println!("tag command: {command}");
                }
                if let Some(diff) = report.index_diff.as_deref() {
                    println!("\nindex diff:\n{diff}");
                }
                if let Some(url) = report.index_pr_url.as_deref() {
                    println!("index PR: {url}");
                }
                println!("artifact: {}", report.artifact_dir);
                println!("files: {}", report.files.len());
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn publish_rule_package(
    anchor: Option<&Path>,
    dry_run: bool,
    remote: &str,
    index_repo: &str,
    index_path: &Path,
    registry_name: Option<&str>,
    skip_index_pr: bool,
    registry: Option<&str>,
    json: bool,
) {
    let options = PackagePublishOptions {
        dry_run,
        remote,
        index_repo,
        index_path,
        registry_name,
        skip_index_pr,
        registry,
    };

    match publish_rule_package_impl(anchor, &options) {
        Ok(report) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#))
                );
            } else {
                if report.dry_run {
                    println!(
                        "Rule-pack publish dry run to {} succeeded.",
                        report.registry
                    );
                } else {
                    println!("Published rule pack {}.", report.tag);
                }
                println!("tag: {}", report.tag);
                println!("sha: {}", report.sha);
                if let Some(command) = report.tag_command.as_deref() {
                    println!("tag command: {command}");
                }
                if let Some(diff) = report.index_diff.as_deref() {
                    println!("\nindex diff:\n{diff}");
                }
                if let Some(url) = report.index_pr_url.as_deref() {
                    println!("index PR: {url}");
                }
                println!("artifact: {}", report.artifact_dir);
                println!("files: {}", report.files.len());
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn list_packages(json: bool) {
    match list_packages_impl() {
        Ok(report) if json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&report)
                    .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#))
            );
        }
        Ok(report) => print_package_list_report(&report),
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn doctor_packages(json: bool) {
    match doctor_packages_impl() {
        Ok(report) if json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&report)
                    .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#))
            );
            if !report.ok {
                process::exit(1);
            }
        }
        Ok(report) => {
            print_package_doctor_report(&report);
            if !report.ok {
                process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}
