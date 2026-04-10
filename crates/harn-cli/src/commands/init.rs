use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use crate::cli::ProjectTemplate;

pub(crate) fn init_project(name: Option<&str>, template: ProjectTemplate) {
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

    let project_name = name.unwrap_or("my-project");
    for (relative_path, content) in template_files(project_name, template) {
        write_if_new(&dir.join(relative_path), &content);
    }

    println!();
    if let Some(n) = name {
        println!("  cd {}", n);
    }
    match template {
        ProjectTemplate::Basic | ProjectTemplate::Agent | ProjectTemplate::Eval => {
            println!("  harn run main.harn       # run the program");
            println!("  harn test tests/         # run the tests");
        }
        ProjectTemplate::McpServer => {
            println!("  harn mcp-serve main.harn # expose the starter MCP server");
        }
    }
    println!("  harn fmt main.harn       # format code");
    println!("  harn lint main.harn      # lint code");
    println!("  harn doctor              # verify the local environment");
}

fn write_if_new(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("Failed to create {}: {e}", parent.display());
            return;
        }
    }
    if path.exists() {
        println!("  skip  {} (already exists)", path.display());
    } else {
        fs::write(path, content).unwrap_or_else(|e| {
            eprintln!("Failed to write {}: {e}", path.display());
        });
        println!("  create  {}", path.display());
    }
}

fn template_files(project_name: &str, template: ProjectTemplate) -> Vec<(&'static str, String)> {
    let manifest = format!(
        r#"[package]
name = "{project_name}"
version = "0.1.0"

[dependencies]
"#
    );

    match template {
        ProjectTemplate::Basic => vec![
            ("harn.toml", manifest),
            (
                "main.harn",
                r#"import "lib/helpers"

pipeline default(task) {
  let greeting = greet("world")
  log(greeting)
}
"#
                .to_string(),
            ),
            (
                "lib/helpers.harn",
                r#"fn greet(name) {
  return "Hello, " + name + "!"
}

fn add(a, b) {
  return a + b
}
"#
                .to_string(),
            ),
            (
                "tests/test_main.harn",
                r#"import "../lib/helpers"

pipeline test_greet(task) {
  assert_eq(greet("world"), "Hello, world!")
  assert_eq(greet("Harn"), "Hello, Harn!")
}

pipeline test_add(task) {
  assert_eq(add(2, 3), 5)
  assert_eq(add(-1, 1), 0)
  assert_eq(add(0, 0), 0)
}
"#
                .to_string(),
            ),
        ],
        ProjectTemplate::Agent => vec![
            ("harn.toml", manifest),
            (
                "main.harn",
                r#"pipeline default(task) {
  var tools = tool_registry()
  tools = tool_define(tools, "read_repo_file", "Read a file from the current repository", {
    parameters: {
      type: "object",
      properties: {
        path: {type: "string"}
      },
      required: ["path"]
    },
    returns: {type: "string"},
    handler: fn(args) {
      return read_file(args.path)
    }
  })

  let result = agent_loop(task, "You are a careful coding agent. Read the repository before proposing changes.", {
    persistent: true,
    max_nudges: 3,
    tools: tools
  })

  println(result.text)
}
"#
                .to_string(),
            ),
            (
                "tests/test_agent.harn",
                r###"pipeline test_agent_smoke(task) {
  llm_mock({text: "##DONE##\nRepository looks healthy."})
  let result = agent_loop("Review the repository", "You are a code review agent.", {
    max_nudges: 1
  })

  assert_eq(result.status, "completed")
}
"###
                .to_string(),
            ),
        ],
        ProjectTemplate::McpServer => vec![
            ("harn.toml", manifest),
            (
                "main.harn",
                r#"pipeline default(task) {
  var tools = tool_registry()
  tools = tool_define(tools, "ping", "Return a pong response", {
    parameters: {
      type: "object",
      properties: {
        message: {type: "string"}
      },
      required: ["message"]
    },
    returns: {
      type: "object",
      properties: {
        message: {type: "string"},
        echoed: {type: "string"}
      }
    },
    handler: fn(args) {
      return {
        message: "pong",
        echoed: args.message
      }
    }
  })

  mcp_tools(tools)
  mcp_resource({
    uri: "info://server",
    name: "server-info",
    text: "Harn MCP starter server"
  })
}
"#
                .to_string(),
            ),
        ],
        ProjectTemplate::Eval => vec![
            ("harn.toml", manifest),
            (
                "main.harn",
                r#"pipeline default(task) {
  let input = "hello world"
  let output = input.upper()
  let passed = output == "HELLO WORLD"

  eval_metric("passed", passed)
  eval_metric("output_length", len(output))

  println(json_stringify({
    input: input,
    output: output,
    passed: passed
  }))
}
"#
                .to_string(),
            ),
            (
                "tests/test_eval.harn",
                r#"pipeline test_eval_metrics(task) {
  eval_metric("accuracy", 1.0, {suite: "smoke"})
  let metrics = eval_metrics()

  assert_eq(len(metrics), 1)
  assert_eq(metrics[0].name, "accuracy")
}
"#
                .to_string(),
            ),
            (
                "eval-suite.json",
                r#"{
  "_type": "eval_suite_manifest",
  "id": "sample-suite",
  "name": "Sample Eval Suite",
  "base_dir": ".",
  "cases": [
    {
      "label": "replace-with-a-run-record",
      "run_path": ".harn-runs/sample-run.json"
    }
  ]
}
"#
                .to_string(),
            ),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::template_files;
    use crate::cli::ProjectTemplate;

    #[test]
    fn basic_template_keeps_library_layout() {
        let files = template_files("sample", ProjectTemplate::Basic);
        let paths: Vec<&str> = files.iter().map(|(path, _)| *path).collect();
        assert!(paths.contains(&"lib/helpers.harn"));
        assert!(paths.contains(&"tests/test_main.harn"));
    }

    #[test]
    fn new_templates_include_expected_entrypoints() {
        let agent = template_files("sample", ProjectTemplate::Agent);
        assert!(agent.iter().any(|(path, _)| *path == "main.harn"));
        assert!(agent
            .iter()
            .any(|(path, _)| *path == "tests/test_agent.harn"));

        let mcp = template_files("sample", ProjectTemplate::McpServer);
        assert!(mcp.iter().any(|(path, _)| *path == "main.harn"));

        let eval = template_files("sample", ProjectTemplate::Eval);
        assert!(eval.iter().any(|(path, _)| *path == "eval-suite.json"));
    }
}
