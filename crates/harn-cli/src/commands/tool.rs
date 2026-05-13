use std::fs;
use std::path::{Path, PathBuf};

use crate::cli::ToolNewArgs;
use crate::package::{
    current_harn_range_example, generate_package_docs_impl, toml_string_literal,
    validate_package_alias, PackageError,
};

pub(crate) fn run_new(args: &ToolNewArgs) -> Result<(), PackageError> {
    validate_package_alias(&args.name)?;
    let dest = args
        .dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&args.name));
    if dest.exists() {
        if !args.force {
            return Err(format!(
                "{} already exists. Pass --force to overwrite.",
                dest.display()
            )
            .into());
        }
        if !dest.is_dir() {
            return Err(format!("{} exists and is not a directory.", dest.display()).into());
        }
    } else {
        fs::create_dir_all(&dest)
            .map_err(|error| format!("failed to create {}: {error}", dest.display()))?;
    }

    let description = args
        .description
        .clone()
        .unwrap_or_else(|| format!("Custom Harn tool package for {}.", args.name));
    let ident = harn_identifier(&args.name)?;
    let handler = format!("handle_{ident}");

    for (relative_path, content) in tool_template_files(&args.name, &ident, &handler, &description)?
    {
        write_file(&dest, relative_path, &content)?;
    }
    generate_package_docs_impl(Some(&dest), None, false)?;

    println!(
        "Scaffolded tool package '{}' at {}",
        args.name,
        dest.display()
    );
    println!("  harn test tests/");
    println!("  harn package check");
    println!("  harn package docs --check");
    println!("  harn package pack --dry-run");
    Ok(())
}

fn tool_template_files(
    package_name: &str,
    ident: &str,
    handler: &str,
    description: &str,
) -> Result<Vec<(&'static str, String)>, PackageError> {
    let harn_range = current_harn_range_example();
    let package_name_toml = toml_string_literal(package_name)?;
    let description_toml = toml_string_literal(description)?;
    let repository = toml_string_literal(&format!("https://github.com/OWNER/{package_name}"))?;
    let provenance = toml_string_literal(&format!(
        "https://github.com/OWNER/{package_name}/releases/tag/v0.1.0"
    ))?;
    let tool_name_harn = harn_string_literal(package_name);
    let description_harn = harn_string_literal(description);
    let handler_doc = description.trim_end_matches('.');

    Ok(vec![
        (
            "harn.toml",
            format!(
                r#"[package]
name = {package_name_toml}
version = "0.1.0"
description = {description_toml}
license = "MIT OR Apache-2.0"
repository = {repository}
provenance = {provenance}
harn = "{harn_range}"
docs_url = "docs/api.md"
permissions = ["tool:read_only"]

[exports]
tools = "lib/tools.harn"

[[package.tools]]
name = {package_name_toml}
module = "lib/tools.harn"
symbol = "tools"
description = {description_toml}
permissions = ["tool:read_only"]

[package.tools.input_schema]
type = "object"
required = ["text"]

[package.tools.input_schema.properties.text]
type = "string"
description = "Text to echo."

[package.tools.output_schema]
type = "string"

[package.tools.annotations]
kind = "read"
side_effect_level = "read_only"

[package.tools.annotations.arg_schema]
required = ["text"]

[dependencies]
"#
            ),
        ),
        (
            "lib/tools.harn",
            format!(
                r#"/** {handler_doc}. */
pub fn {handler}(args) {{
  let input = schema_expect(args, {{
    type: "object",
    properties: {{
      text: {{type: "string"}}
    }},
    required: ["text"]
  }})
  return input.text
}}

/** Return a tool registry containing `{package_name}`. */
pub fn tools(registry = nil) {{
  var reg = registry ?? tool_registry()
  reg = tool_define(reg, {tool_name_harn}, {description_harn}, {{
    parameters: {{
      text: {{
        type: "string",
        description: "Text to echo."
      }}
    }},
    returns: {{type: "string"}},
    annotations: {{
      kind: "read",
      side_effect_level: "read_only",
      arg_schema: {{required: ["text"]}}
    }},
    handler: {handler}
  }})
  return reg
}}
"#
            ),
        ),
        (
            "main.harn",
            format!(
                r#"import {{ agent_dispatch_tool_call }} from "std/agent/primitives"
import {{ tools }} from "lib/tools"

pipeline default() {{
  let registry = tools()
  var text = "hello"
  if len(argv) > 0 {{
    text = argv[0]
  }}
  let result = agent_dispatch_tool_call({{
    name: {tool_name_harn},
    arguments: {{text: text}}
  }}, registry)
  if !result.ok {{
    throw (result.error?.message ?? "tool call failed")
  }}
  println(result.rendered_result)
}}
"#
            ),
        ),
        (
            "tests/test_tool.harn",
            format!(
                r#"import {{ agent_dispatch_tool_call }} from "std/agent/primitives"
import {{ tools }} from "../lib/tools"

pipeline test_{ident}_tool(task) {{
  let registry = tools()
  let ok = agent_dispatch_tool_call({{
    name: {tool_name_harn},
    arguments: {{text: "hello"}}
  }}, registry)
  assert(ok.ok, ok.error?.message ?? "tool call should pass")
  assert_eq(ok.rendered_result, "hello")

  let bad = agent_dispatch_tool_call({{
    name: {tool_name_harn},
    arguments: {{}}
  }}, registry)
  assert(!bad.ok, "schema validation should reject missing text")
}}
"#
            ),
        ),
        (
            "README.md",
            format!(
                r#"# {package_name}

{description}

## Develop

```bash
harn test tests/
harn package check
harn package docs --check
harn package pack --dry-run
```

## Install into another project

```bash
harn add ../{package_name}
harn install
```

Consumers import the stable registry builder with:

```harn
import {{ tools }} from "{package_name}/tools"
```
"#
            ),
        ),
        ("LICENSE", "MIT OR Apache-2.0\n".to_string()),
        (
            ".github/workflows/harn-package.yml",
            r#"name: Harn tool package

on:
  pull_request:
  push:
    branches: [main]

jobs:
  package:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: taiki-e/install-action@cargo-binstall
      - run: cargo binstall harn-cli --no-confirm
      - run: harn install --locked --offline || harn install
      - run: harn test tests/
      - run: harn package check
      - run: harn package docs --check
      - run: harn package pack --dry-run
"#
            .to_string(),
        ),
    ])
}

fn write_file(root: &Path, relative_path: &str, content: &str) -> Result<(), PackageError> {
    let path = root.join(relative_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    harn_vm::atomic_io::atomic_write(&path, content.as_bytes())
        .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
    Ok(())
}

fn harn_identifier(name: &str) -> Result<String, PackageError> {
    let mut out = String::new();
    for ch in name.chars() {
        if ch == '_' || ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    let out = out.trim_matches('_').to_string();
    if out.is_empty() {
        return Err(format!("tool name {name:?} does not contain an identifier").into());
    }
    if out
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
    {
        Ok(out)
    } else {
        Ok(format!("tool_{out}"))
    }
}

fn harn_string_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harn_identifier_normalizes_package_names() {
        assert_eq!(harn_identifier("acme-tool").unwrap(), "acme_tool");
        assert_eq!(harn_identifier("123-tool").unwrap(), "tool_123_tool");
    }

    #[test]
    fn force_overwrites_templates_without_deleting_extra_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("acme-tool");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("keep.txt"), "keep").unwrap();

        run_new(&ToolNewArgs {
            name: "acme-tool".to_string(),
            description: None,
            dir: Some(dest.display().to_string()),
            force: true,
        })
        .unwrap();

        assert_eq!(fs::read_to_string(dest.join("keep.txt")).unwrap(), "keep");
        assert!(dest.join("harn.toml").exists());
    }
}
