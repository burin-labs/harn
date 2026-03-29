use std::fs;
use std::path::{Path, PathBuf};
use std::process;

pub(crate) fn init_project(name: Option<&str>) {
    let dir = match name {
        Some(n) => {
            let dir = PathBuf::from(n);
            if dir.exists() {
                eprintln!("Directory '{}' already exists", n);
                process::exit(1);
            }
            fs::create_dir_all(&dir).unwrap_or_else(|e| {
                eprintln!("Failed to create directory: {e}");
                process::exit(1);
            });
            println!("Creating project '{}'...", n);
            dir
        }
        None => {
            println!("Initializing harn project in current directory...");
            PathBuf::from(".")
        }
    };

    // Create directories
    fs::create_dir_all(dir.join("lib")).ok();
    fs::create_dir_all(dir.join("tests")).ok();

    // main.harn
    let main_content = r#"import "lib/helpers"

pipeline default(task) {
  let greeting = greet("world")
  log(greeting)
}
"#;

    // lib/helpers.harn
    let helpers_content = r#"fn greet(name) {
  return "Hello, " + name + "!"
}

fn add(a, b) {
  return a + b
}
"#;

    // tests/test_main.harn
    let test_content = r#"import "../lib/helpers"

pipeline test_greet(task) {
  assert_eq(greet("world"), "Hello, world!")
  assert_eq(greet("Harn"), "Hello, Harn!")
}

pipeline test_add(task) {
  assert_eq(add(2, 3), 5)
  assert_eq(add(-1, 1), 0)
  assert_eq(add(0, 0), 0)
}
"#;

    // harn.toml
    let project_name = name.unwrap_or("my-project");
    let manifest_content = format!(
        r#"[package]
name = "{project_name}"
version = "0.1.0"

[dependencies]
"#
    );

    // Write files (don't overwrite existing)
    write_if_new(&dir.join("harn.toml"), &manifest_content);
    write_if_new(&dir.join("main.harn"), main_content);
    write_if_new(&dir.join("lib/helpers.harn"), helpers_content);
    write_if_new(&dir.join("tests/test_main.harn"), test_content);

    println!();
    if let Some(n) = name {
        println!("  cd {}", n);
    }
    println!("  harn run main.harn       # run the program");
    println!("  harn test tests/         # run the tests");
    println!("  harn fmt main.harn       # format code");
    println!("  harn lint main.harn      # lint code");
}

fn write_if_new(path: &Path, content: &str) {
    if path.exists() {
        println!("  skip  {} (already exists)", path.display());
    } else {
        fs::write(path, content).unwrap_or_else(|e| {
            eprintln!("Failed to write {}: {e}", path.display());
        });
        println!("  create  {}", path.display());
    }
}
