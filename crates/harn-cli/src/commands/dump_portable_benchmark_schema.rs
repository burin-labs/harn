//! Generate the public benchmark receipt schema from `harn-kernel`.

use crate::commands::generated_artifact::write_or_check;
use std::path::Path;

pub(crate) fn run(output_path: &str, check_only: bool) {
    let generated = format!(
        "{}\n",
        serde_json::to_string_pretty(&harn_kernel::portable_benchmark_json_schema())
            .expect("portable benchmark schema is JSON serializable")
    );
    write_or_check(
        Path::new(output_path),
        &generated,
        check_only,
        "the kernel receipt contract",
        "gen-portable-benchmark-schema",
    );
}
