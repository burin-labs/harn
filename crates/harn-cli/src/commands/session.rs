use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;

use crate::cli::{
    SessionArgs, SessionCommand, SessionExportArgs, SessionImportArgs, SessionSchemaArgs,
    SessionValidateArgs,
};

const DEFAULT_SCHEMA_PATH: &str = "spec/schemas/session-bundle.v1.schema.json";

pub(crate) fn run(args: SessionArgs) {
    match args.command {
        SessionCommand::Export(export) => run_export(export),
        SessionCommand::Import(import) => run_import(import),
        SessionCommand::Validate(validate) => run_validate(validate),
        SessionCommand::Schema(schema) => run_schema(schema),
    }
}

fn run_export(args: SessionExportArgs) {
    let run = match harn_vm::orchestration::load_run_record(Path::new(&args.run_record)) {
        Ok(run) => run,
        Err(error) => {
            eprintln!(
                "error: failed to load run record {}: {error}",
                args.run_record
            );
            process::exit(1);
        }
    };
    let mode = if args.local {
        harn_vm::session_bundle::SessionBundleExportMode::Local
    } else if args.replay_only {
        harn_vm::session_bundle::SessionBundleExportMode::ReplayOnly
    } else {
        harn_vm::session_bundle::SessionBundleExportMode::Sanitized
    };
    let options = harn_vm::session_bundle::SessionBundleExportOptions {
        mode,
        include_attachments: args.include_attachments,
        ..Default::default()
    };
    let bundle = match harn_vm::session_bundle::export_run_record_bundle(&run, &options) {
        Ok(bundle) => bundle,
        Err(error) => {
            eprintln!("error: failed to export session bundle: {error}");
            process::exit(1);
        }
    };
    let rendered = match serde_json::to_string_pretty(&bundle) {
        Ok(json) => format!("{json}\n"),
        Err(error) => {
            eprintln!("error: failed to render session bundle: {error}");
            process::exit(1);
        }
    };
    if let Some(out) = args.out {
        write_text(Path::new(&out), &rendered);
        println!("{out}");
    } else {
        write_stdout(&rendered);
    }
}

fn run_import(args: SessionImportArgs) {
    let bundle = read_validated_bundle(
        &args.bundle,
        args.allow_unsafe_secret_markers,
        "invalid session bundle",
    );
    let run_record = match harn_vm::session_bundle::import_run_record_value(&bundle) {
        Ok(run_record) => run_record,
        Err(error) => {
            eprintln!("error: failed to import session bundle: {error}");
            process::exit(1);
        }
    };
    let rendered = match serde_json::to_string_pretty(&run_record) {
        Ok(json) => format!("{json}\n"),
        Err(error) => {
            eprintln!("error: failed to render imported run record: {error}");
            process::exit(1);
        }
    };
    let out = args.out.map(PathBuf::from).unwrap_or_else(|| {
        harn_vm::runtime_paths::run_root(Path::new("."))
            .join("imported")
            .join(format!("{}.json", bundle.source.run_record_id))
    });
    write_text(&out, &rendered);
    println!("{}", out.display());
}

fn run_validate(args: SessionValidateArgs) {
    let bundle = read_validated_bundle(
        &args.bundle,
        args.allow_unsafe_secret_markers,
        "invalid session bundle",
    );
    if args.json {
        let summary = serde_json::json!({
            "ok": true,
            "bundle_id": bundle.bundle_id,
            "schema_version": bundle.schema_version,
            "mode": bundle.redaction.mode,
            "run_record_id": bundle.source.run_record_id,
        });
        match serde_json::to_string_pretty(&summary) {
            Ok(json) => println!("{json}"),
            Err(error) => {
                eprintln!("error: failed to render validation summary: {error}");
                process::exit(1);
            }
        }
    } else {
        println!(
            "OK {} schema_version={} mode={}",
            bundle.bundle_id, bundle.schema_version, bundle.redaction.mode
        );
    }
}

fn run_schema(args: SessionSchemaArgs) {
    let rendered = match harn_vm::session_bundle::session_bundle_schema_pretty() {
        Ok(schema) => schema,
        Err(error) => {
            eprintln!("error: failed to render session bundle schema: {error}");
            process::exit(1);
        }
    };
    let write_to_file = args.out.is_some();
    let path = args
        .out
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SCHEMA_PATH));
    if args.check {
        match fs::read_to_string(&path) {
            Ok(existing)
                if normalize_line_endings(&existing) == normalize_line_endings(&rendered) =>
            {
                return;
            }
            Ok(_) => {
                eprintln!(
                    "error: {} is stale. Run `make gen-session-bundle-schema` to regenerate.",
                    path.display()
                );
                process::exit(1);
            }
            Err(error) => {
                eprintln!("error: failed to read {}: {error}", path.display());
                process::exit(1);
            }
        }
    }
    if write_to_file {
        write_text(&path, &rendered);
        println!("{}", path.display());
    } else {
        write_stdout(&rendered);
    }
}

fn read_validated_bundle(
    path: &str,
    allow_unsafe_secret_markers: bool,
    context: &str,
) -> harn_vm::session_bundle::SessionBundle {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) => {
            eprintln!("error: failed to read session bundle {path}: {error}");
            process::exit(1);
        }
    };
    let options = harn_vm::session_bundle::SessionBundleValidationOptions {
        allow_unsafe_secret_markers,
        ..Default::default()
    };
    match harn_vm::session_bundle::validate_session_bundle_str(&content, &options) {
        Ok(bundle) => bundle,
        Err(error) => {
            eprintln!("error: {context}: {error}");
            process::exit(1);
        }
    }
}

fn write_text(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(error) = fs::create_dir_all(parent) {
                eprintln!("error: failed to create {}: {error}", parent.display());
                process::exit(1);
            }
        }
    }
    if let Err(error) = fs::write(path, content) {
        eprintln!("error: failed to write {}: {error}", path.display());
        process::exit(1);
    }
}

fn write_stdout(content: &str) {
    if let Err(error) = io::stdout().write_all(content.as_bytes()) {
        eprintln!("error: failed to write stdout: {error}");
        process::exit(1);
    }
}

fn normalize_line_endings(input: &str) -> String {
    input.replace("\r\n", "\n")
}
