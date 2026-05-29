//! `harn pg codegen` — generate Harn record types from `.sql` migrations.
//!
//! Walks a migration directory, replays every forward (`.sql`, non-`.down.sql`)
//! migration in lexicographic order — the same discovery rule `pg_migrate`
//! uses at runtime — and emits one `type <Table>Row = {…}` per table whose
//! columns mirror the live schema. The generated file lets callers annotate a
//! query result (`let r: ReceiptRow = pg_query_one(pool, sql, params)`) so the
//! type-checker proves every downstream field access against the schema on
//! disk, with no live database round-trip.
//!
//! `--check` re-renders and compares against the existing `--out` file without
//! writing, exiting non-zero on drift — wire it into CI to keep generated
//! types synchronized with the migrations.

mod ddl;
mod emit;

use std::path::{Path, PathBuf};

use crate::cli::PgCodegenArgs;

const DEFAULT_TYPE_SUFFIX: &str = "Row";

pub(crate) fn run(args: &PgCodegenArgs) -> i32 {
    match run_inner(args) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("harn pg codegen: {message}");
            1
        }
    }
}

fn run_inner(args: &PgCodegenArgs) -> Result<i32, String> {
    let sources = read_migrations(&args.dir)?;
    let schema = ddl::parse_migrations(&sources);
    let suffix = args.suffix.as_deref().unwrap_or(DEFAULT_TYPE_SUFFIX);
    let header = header_for(&args.dir, args.out.as_deref(), suffix);
    let rendered = emit::render(&schema, &header, suffix);

    let Some(out) = args.out.as_deref() else {
        print!("{rendered}");
        return Ok(0);
    };

    if args.check {
        return Ok(check_against(out, &rendered));
    }

    harn_vm::atomic_io::atomic_write(out, rendered.as_bytes())
        .map_err(|error| format!("failed to write {}: {error}", out.display()))?;
    let table_count = schema.tables().count();
    eprintln!(
        "wrote {table_count} type{} to {}",
        if table_count == 1 { "" } else { "s" },
        out.display()
    );
    Ok(0)
}

/// Read every forward migration in `dir`, lexicographically ordered.
fn read_migrations(dir: &Path) -> Result<Vec<String>, String> {
    if !dir.exists() {
        return Err(format!(
            "migrations directory does not exist: {}",
            dir.display()
        ));
    }
    let read_dir = std::fs::read_dir(dir)
        .map_err(|error| format!("could not read {}: {error}", dir.display()))?;
    let mut paths: Vec<PathBuf> = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_forward_migration(path))
        .collect();
    paths.sort();

    paths
        .iter()
        .map(|path| {
            std::fs::read_to_string(path)
                .map_err(|error| format!("could not read {}: {error}", path.display()))
        })
        .collect()
}

/// A forward migration is a `.sql` file that is not a `.down.sql` rollback —
/// the same rule `pg_migrate` applies when discovering migrations to run.
fn is_forward_migration(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name.ends_with(".sql") && !name.ends_with(".down.sql")
}

fn check_against(out: &Path, rendered: &str) -> i32 {
    match std::fs::read_to_string(out) {
        Ok(existing) if existing == rendered => 0,
        Ok(_) => {
            eprintln!(
                "{} is out of date; regenerate with `harn pg codegen`",
                out.display()
            );
            1
        }
        Err(_) => {
            eprintln!(
                "{} is missing; generate it with `harn pg codegen`",
                out.display()
            );
            1
        }
    }
}

fn header_for(dir: &Path, out: Option<&Path>, suffix: &str) -> String {
    let mut cmd = format!("harn pg codegen --dir {}", dir.display());
    if let Some(out) = out {
        cmd.push_str(&format!(" --out {}", out.display()));
    }
    if suffix != DEFAULT_TYPE_SUFFIX {
        cmd.push_str(&format!(" --suffix {suffix}"));
    }
    format!(
        "// Code generated from SQL migrations by `harn pg codegen`. DO NOT EDIT.\n\
         // Regenerate with: {cmd}\n\n"
    )
}

#[cfg(test)]
mod tests;
