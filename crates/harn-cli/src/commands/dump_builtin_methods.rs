//! `harn dump-builtin-methods` — regenerate the builtin method reference in
//! `docs/src/builtins.md` from the method registry
//! (`harn_parser::builtin_methods`).
//!
//! Only the region between the generated markers is rewritten. With
//! `--check`, the command exits non-zero when that region is stale.

use std::fs;
use std::process;

use harn_parser::builtin_methods::{splice_reference, REFERENCE_BEGIN_MARKER};

pub(crate) fn run(output_path: &str, check_only: bool) {
    let existing = match fs::read_to_string(output_path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("error: cannot read {output_path}: {error}");
            process::exit(1);
        }
    };
    let Some(generated) = splice_reference(&existing) else {
        eprintln!("error: {output_path} has no `{REFERENCE_BEGIN_MARKER}` region");
        process::exit(1);
    };
    if check_only {
        if generated.replace("\r\n", "\n") != existing.replace("\r\n", "\n") {
            eprintln!("error: {output_path} is stale relative to the builtin method registry.");
            eprintln!("hint: run `make gen-builtin-methods` to regenerate.");
            process::exit(1);
        }
        return;
    }
    if let Err(error) = fs::write(output_path, generated) {
        eprintln!("error: cannot write {output_path}: {error}");
        process::exit(1);
    }
    println!("wrote {output_path}");
}
