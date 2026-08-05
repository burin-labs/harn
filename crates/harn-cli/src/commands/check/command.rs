use std::path::{Path, PathBuf};
use std::process;

use crate::cli::{CheckArgs, CheckOutputFormat};
use crate::{command_error, json_envelope, package, print_check_error};

use super::{
    build_module_graph_with_parsed_sources, check_files, check_files_independently,
    collect_cross_file_imports, collect_harn_targets, connector_matrix, provider_matrix,
    CheckCliOverrides, CheckReport, CHECK_SCHEMA_VERSION,
};

pub(crate) fn run_check_command(args: CheckArgs) {
    let json_format_alias = !args.json && matches!(args.format, CheckOutputFormat::Json);
    let matrix_format = if args.json {
        if !matches!(args.format, CheckOutputFormat::Text) {
            command_error("`harn check` accepts either `--json` or `--format`, not both");
        }
        CheckOutputFormat::Json
    } else {
        args.format
    };
    if args.provider_matrix {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let extensions = package::load_runtime_extensions(&cwd);
        package::install_runtime_extensions(&extensions);
        provider_matrix::run(matrix_format, args.filter.as_deref(), json_format_alias);
        return;
    }
    if args.connector_matrix {
        connector_matrix::run(
            matrix_format,
            args.filter.as_deref(),
            &args.targets,
            json_format_alias,
        );
        return;
    }

    let mut target_strings = args.targets.clone();
    if args.workspace {
        append_workspace_pipelines(&mut target_strings);
    }
    if target_strings.is_empty() {
        check_input_error(
            args.json,
            "missing_targets",
            "`harn check` requires at least one target path, or `--workspace` with `[workspace].pipelines`",
        );
    }
    validate_runtime_manifests(&target_strings, args.json);

    let targets: Vec<&str> = target_strings.iter().map(String::as_str).collect();
    let files = collect_harn_targets(&targets);
    if files.is_empty() {
        check_input_error(
            args.json,
            "no_harn_files",
            "no .harn or .harn.txt files found under the given target(s)",
        );
    }

    let overrides = CheckCliOverrides::from(&args);
    let checked = if args.independent {
        check_files_independently(&files, &overrides, !args.json)
    } else {
        let (module_graph, parsed_sources) = build_module_graph_with_parsed_sources(&files);
        let cross_file_imports = collect_cross_file_imports(&module_graph);
        check_files(
            &files,
            &module_graph,
            parsed_sources,
            &cross_file_imports,
            &overrides,
            !args.json,
        )
    };

    let mut should_fail = false;
    let mut json_files = Vec::new();
    for checked_file in checked {
        should_fail |= checked_file
            .report
            .outcome()
            .should_fail(checked_file.strict);
        if args.json {
            json_files.push(checked_file.report);
        } else {
            checked_file.text.print();
        }
    }
    if args.json {
        print_json_report(json_files, should_fail);
    }
    if should_fail {
        process::exit(1);
    }
}

fn append_workspace_pipelines(targets: &mut Vec<String>) {
    let anchor = targets.first().map(Path::new);
    match package::load_workspace_config(anchor) {
        Some((workspace, manifest_dir)) if !workspace.pipelines.is_empty() => {
            for pipeline in workspace.pipelines {
                let candidate = Path::new(&pipeline);
                let resolved = if candidate.is_absolute() {
                    candidate.to_path_buf()
                } else {
                    manifest_dir.join(candidate)
                };
                targets.push(resolved.to_string_lossy().into_owned());
            }
        }
        Some(_) => {
            command_error("--workspace requires `[workspace].pipelines` in the nearest harn.toml")
        }
        None => {
            command_error("--workspace could not find a harn.toml walking up from the target(s)")
        }
    }
}

fn validate_runtime_manifests(targets: &[String], json: bool) {
    for target in targets {
        if let Err(error) = package::validate_runtime_manifest_extensions(Path::new(target)) {
            check_input_error(
                json,
                "manifest_extension_error",
                &format!("manifest extension validation failed: {error}"),
            );
        }
    }
}

fn check_input_error(json: bool, code: &str, message: &str) -> ! {
    if json {
        print_check_error(code, message);
    }
    command_error(message);
}

fn print_json_report(files: Vec<super::check_cmd::CheckFileReport>, should_fail: bool) {
    let report = CheckReport::from_files(files);
    let envelope = if should_fail {
        json_envelope::JsonEnvelope {
            schema_version: CHECK_SCHEMA_VERSION,
            ok: false,
            data: Some(report),
            error: Some(json_envelope::JsonError {
                code: "check_failed".to_string(),
                message: "one or more files failed `harn check`".to_string(),
                details: serde_json::Value::Null,
            }),
            warnings: Vec::new(),
        }
    } else {
        json_envelope::JsonEnvelope::ok(CHECK_SCHEMA_VERSION, report)
    };
    println!("{}", json_envelope::to_string_pretty(&envelope));
    if should_fail {
        process::exit(1);
    }
}
