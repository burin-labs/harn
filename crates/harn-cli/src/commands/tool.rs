use std::fs;
use std::path::PathBuf;

use crate::cli::ToolNewArgs;
use crate::commands::scaffold_common::{
    harn_identifier_with_prefix, harn_string_literal, write_file,
};
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;
use crate::package::{
    current_harn_range_example, generate_package_docs_impl, toml_string_literal,
    validate_package_alias, PackageError,
};

/// `harn tool new` dispatch shim. Validates the requested package alias
/// in Rust, resolves the destination directory, then delegates the
/// template render + file-write loop to `cli/scaffold/tool_new.harn`
/// (harn#2308 / W8 of the harn-cli self-host epic). The legacy Rust
/// implementation stays behind `HARN_CLI_IMPL=rust` until the C1 ratchet
/// (#2314) removes it.
pub(crate) async fn run_new(args: &ToolNewArgs) -> Result<(), PackageError> {
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
    if description.contains('\n') || description.contains('\r') {
        return Err("tool description must be a single line".to_string().into());
    }
    let ident = harn_identifier(&args.name)?;
    let handler = format!("handle_{ident}");

    if std::env::var("HARN_CLI_IMPL").as_deref() == Ok("rust") {
        run_new_rust(&args.name, &dest, &ident, &handler, &description)?;
    } else {
        dispatch_to_script(&args.name, &dest, &ident, &handler, &description).await?;
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

async fn dispatch_to_script(
    name: &str,
    dest: &std::path::Path,
    ident: &str,
    handler: &str,
    description: &str,
) -> Result<(), PackageError> {
    let dest_str = dest.display().to_string();
    let harn_range = current_harn_range_example();
    let _name_env = ScopedEnvVar::set("HARN_TOOL_NAME", name);
    let _dest_env = ScopedEnvVar::set("HARN_TOOL_DEST", &dest_str);
    let _ident_env = ScopedEnvVar::set("HARN_TOOL_IDENT", ident);
    let _handler_env = ScopedEnvVar::set("HARN_TOOL_HANDLER", handler);
    let _desc_env = ScopedEnvVar::set("HARN_TOOL_DESCRIPTION", description);
    let _range_env = ScopedEnvVar::set("HARN_TOOL_HARN_RANGE", &harn_range);
    let exit = dispatch::dispatch_to_embedded_script(
        "scaffold/tool_new",
        Vec::new(),
        /* json_mode */ false,
    )
    .await;
    if exit != 0 {
        return Err(format!("tool new scaffolder exited with code {exit}").into());
    }
    Ok(())
}

/// Legacy Rust implementation. Kept behind `HARN_CLI_IMPL=rust` for the
/// parity harness (#2299). The C1 ratchet (#2314) removes this path
/// once the `.harn` impl is the production default everywhere.
fn run_new_rust(
    name: &str,
    dest: &std::path::Path,
    ident: &str,
    handler: &str,
    description: &str,
) -> Result<(), PackageError> {
    for (relative_path, content) in tool_template_files(name, ident, handler, description)? {
        write_file(dest, relative_path, &content)?;
    }
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
  log(result.rendered_result)
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

fn harn_identifier(name: &str) -> Result<String, PackageError> {
    harn_identifier_with_prefix(name, "tool")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harn_identifier_normalizes_package_names() {
        assert_eq!(harn_identifier("acme-tool").unwrap(), "acme_tool");
        assert_eq!(harn_identifier("123-tool").unwrap(), "tool_123_tool");
    }

    // The force-overwrite behavior moved to a subprocess parity test in
    // `tests/scaffold_dispatch.rs`: the dispatched `.harn` impl runs the
    // full VM and would overflow the default `#[tokio::test]` thread
    // stack here. The subprocess inherits the workspace-level 16 MiB
    // runtime stack via `harn_cli::install_signal_shutdown_handler`.
}
