//! Generate the public benchmark receipt schema from `harn-kernel`.

use std::fs;
use std::path::Path;
use std::process;

pub(crate) fn run(output_path: &str, check_only: bool) {
    let generated = format!(
        "{}\n",
        serde_json::to_string_pretty(&harn_kernel::portable_benchmark_json_schema())
            .expect("portable benchmark schema is JSON serializable")
    );
    let path = Path::new(output_path);

    if check_only {
        match fs::read_to_string(path) {
            Ok(existing) if normalize_line_endings(&existing) == generated => return,
            Ok(_) => eprintln!(
                "error: {} is stale relative to the kernel receipt contract",
                path.display()
            ),
            Err(error) => eprintln!("error: cannot read {}: {error}", path.display()),
        }
        eprintln!("hint: run `make gen-portable-benchmark-schema` to regenerate");
        process::exit(1);
    }

    if let Some(parent) = path.parent() {
        if let Err(error) = fs::create_dir_all(parent) {
            eprintln!("error: cannot create {}: {error}", parent.display());
            process::exit(1);
        }
    }
    if let Err(error) = fs::write(path, generated) {
        eprintln!("error: cannot write {}: {error}", path.display());
        process::exit(1);
    }
    println!("wrote {}", path.display());
}

fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}
