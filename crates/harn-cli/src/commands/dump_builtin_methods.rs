//! `harn dump-builtin-methods` — regenerate the builtin method reference in
//! `docs/src/builtins.md` from the method registry
//! (`harn_parser::builtin_methods`).
//!
//! Only the region between the generated markers is rewritten. With
//! `--check`, the command exits non-zero when that region is stale.

use std::fs;
use std::path::Path;
use std::process;

use crate::commands::generated_artifact::write_or_check;

use harn_parser::builtin_methods::{splice_reference, REFERENCE_BEGIN_MARKER};

pub(crate) fn run(output_path: &str, check_only: bool) {
    let existing = match fs::read_to_string(output_path) {
        Ok(text) => text,
        Err(error) => {
            cli_diagnostic!("error: cannot read {output_path}: {error}");
            process::exit(1);
        }
    };
    let Some(generated) = splice_reference(&existing) else {
        cli_diagnostic!("error: {output_path} has no `{REFERENCE_BEGIN_MARKER}` region");
        process::exit(1);
    };
    write_or_check(
        Path::new(output_path),
        &generated,
        check_only,
        "the builtin method registry",
        "gen-builtin-methods",
    );
}
