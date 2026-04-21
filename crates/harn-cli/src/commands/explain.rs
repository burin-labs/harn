use crate::cli::ExplainArgs;
use crate::parse_source_file;

pub(crate) fn run_explain(args: &ExplainArgs) -> i32 {
    let (_, program) = parse_source_file(&args.file);
    match harn_ir::explain_handler_invariant(&program, &args.function, &args.invariant) {
        Ok(diagnostics) => {
            if diagnostics.is_empty() {
                println!(
                    "No `{}` violations found for `{}` in {}.",
                    args.invariant, args.function, args.file
                );
                return 0;
            }
            for diagnostic in diagnostics {
                println!(
                    "{}: {} ({})",
                    diagnostic.invariant, diagnostic.message, diagnostic.handler
                );
                for (index, step) in diagnostic.path.iter().enumerate() {
                    println!(
                        "  {}. {}:{}:{} {}",
                        index + 1,
                        args.file,
                        step.span.line,
                        step.span.column,
                        step.label
                    );
                }
                if let Some(help) = &diagnostic.help {
                    println!("  help: {help}");
                }
            }
            0
        }
        Err(message) => {
            eprintln!("{message}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::run_explain;
    use crate::cli::ExplainArgs;

    fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{nanos}"))
    }

    #[test]
    fn explain_returns_zero_for_configured_violation() {
        let dir = unique_temp_dir("harn-explain");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.harn");
        std::fs::write(
            &file,
            r#"
@invariant("approval.reachability")
fn handler() {
  write_file("src/main.rs", "unsafe")
}
"#,
        )
        .unwrap();

        let code = run_explain(&ExplainArgs {
            invariant: "approval.reachability".to_string(),
            function: "handler".to_string(),
            file: file.to_string_lossy().into_owned(),
        });

        assert_eq!(code, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explain_returns_nonzero_for_missing_handler() {
        let dir = unique_temp_dir("harn-explain-missing");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.harn");
        std::fs::write(
            &file,
            r#"
fn handler() {
  log("ok")
}
"#,
        )
        .unwrap();

        let code = run_explain(&ExplainArgs {
            invariant: "approval.reachability".to_string(),
            function: "missing".to_string(),
            file: file.to_string_lossy().into_owned(),
        });

        assert_eq!(code, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
