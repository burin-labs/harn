use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use clap::{Arg, ArgAction, ArgMatches, Command};

use crate::cli::{ToolNewArgs, ToolRunArgs, ToolSchemaArgs};
use crate::commands::run::RunSandboxOptions;
use crate::commands::scaffold_common::harn_identifier_with_prefix;
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;
use crate::package::{
    current_harn_range_example, generate_package_docs_impl, validate_package_alias, PackageError,
};

#[derive(Debug)]
pub(crate) struct ToolCommandError {
    pub(crate) message: String,
    pub(crate) exit_code: i32,
}

impl ToolCommandError {
    fn message(message: String) -> Self {
        Self {
            message,
            exit_code: 1,
        }
    }
}

impl std::fmt::Display for ToolCommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// `harn tool new` dispatch shim. Validates the requested package alias
/// in Rust, resolves the destination directory, then delegates the
/// template render + file-write loop to `cli/scaffold/tool_new.harn`
/// through the standard CLI dispatch path.
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

    dispatch_to_script(&args.name, &dest, &ident, &handler, &description).await?;

    generate_package_docs_impl(Some(&dest), None, false)?;

    println!(
        "Scaffolded tool package '{}' at {}",
        args.name,
        dest.display()
    );
    println!("  harn package verify");
    Ok(())
}

/// Execute one registry entry through a command tree derived from the same
/// normalized catalog that backs MCP discovery.
pub(crate) async fn run_registry(args: &ToolRunArgs) -> Result<(), ToolCommandError> {
    let mut loaded = crate::commands::run::load_file_tool_registry(&args.file)
        .await
        .map_err(|error| ToolCommandError {
            message: error.message,
            exit_code: error.exit_code,
        })?;
    run_loaded_registry(args, &mut loaded)
        .await
        .map_err(ToolCommandError::message)
}

async fn run_loaded_registry(
    args: &ToolRunArgs,
    loaded: &mut crate::commands::run::LoadedToolRegistry,
) -> Result<(), String> {
    if !loaded.diagnostics.is_empty() {
        eprint!("{}", loaded.diagnostics);
    }
    let tools = harn_vm::tool_registry::executable_tools(&loaded.registry)
        .map_err(|error| error.to_string())?;
    let registry_info = harn_vm::tool_registry::tool_registry_catalog(&loaded.registry)
        .map_err(|error| error.to_string())?
        .info;
    let catalog = tools
        .iter()
        .map(|tool| tool.catalog.clone())
        .collect::<Vec<_>>();
    let invocation = match parse_registry_invocation(
        &args.file,
        &args.arguments,
        registry_info.as_ref(),
        &catalog,
    )? {
        Some(invocation) => invocation,
        None => return Ok(()),
    };
    let tool = tools
        .iter()
        .find(|tool| tool.catalog.name == invocation.tool_name)
        .ok_or_else(|| {
            format!(
                "generated command selected unknown tool {:?}",
                invocation.tool_name
            )
        })?;
    let input = harn_vm::schema::json_to_vm_value(&invocation.arguments);
    let result = tokio::task::LocalSet::new()
        .run_until(loaded.vm.call_closure_pub(&tool.handler, &[input]))
        .await
        .map_err(|error| format!("tool {:?} failed: {error}", tool.catalog.name))?;
    let json = harn_vm::tool_registry::result_to_json(&result).map_err(|error| {
        format!(
            "tool {:?} returned a non-JSON value: {error}",
            tool.catalog.name
        )
    })?;
    if let Some(output_schema) = tool.catalog.output_schema.as_ref() {
        let validator = jsonschema::draft202012::new(output_schema).map_err(|error| {
            format!(
                "tool {:?} has an invalid output schema: {error}",
                tool.catalog.name
            )
        })?;
        let violations = validator
            .iter_errors(&json)
            .map(|error| error.to_string())
            .collect::<Vec<_>>();
        if !violations.is_empty() {
            return Err(format!(
                "tool {:?} returned a value that does not match its output schema:\n  - {}",
                tool.catalog.name,
                violations.join("\n  - ")
            ));
        }
    }
    match invocation.output.as_str() {
        "json" => println!(
            "{}",
            serde_json::to_string(&json).map_err(|error| error.to_string())?
        ),
        "pretty" => println!(
            "{}",
            serde_json::to_string_pretty(&json).map_err(|error| error.to_string())?
        ),
        "text" => match json {
            serde_json::Value::String(text) => println!("{text}"),
            other => println!(
                "{}",
                serde_json::to_string_pretty(&other).map_err(|error| error.to_string())?
            ),
        },
        _ => unreachable!("clap validates output values"),
    }
    Ok(())
}

pub(crate) async fn print_registry_schema(args: &ToolSchemaArgs) -> Result<(), ToolCommandError> {
    let loaded = crate::commands::run::load_file_tool_registry(&args.file)
        .await
        .map_err(|error| ToolCommandError {
            message: error.message,
            exit_code: error.exit_code,
        })?;
    print_loaded_registry_schema(args, &loaded).map_err(ToolCommandError::message)
}

fn print_loaded_registry_schema(
    args: &ToolSchemaArgs,
    loaded: &crate::commands::run::LoadedToolRegistry,
) -> Result<(), String> {
    if !loaded.diagnostics.is_empty() {
        eprint!("{}", loaded.diagnostics);
    }
    let catalog = harn_vm::tool_registry::tool_registry_catalog(&loaded.registry)
        .map_err(|error| error.to_string())?;
    if args.pretty {
        println!(
            "{}",
            serde_json::to_string_pretty(&catalog).map_err(|error| error.to_string())?
        );
    } else {
        println!(
            "{}",
            serde_json::to_string(&catalog).map_err(|error| error.to_string())?
        );
    }
    Ok(())
}

#[derive(Default)]
struct CommandNode {
    tool_index: Option<usize>,
    children: BTreeMap<String, CommandNode>,
}

#[derive(Debug)]
struct RegistryInvocation {
    tool_name: String,
    arguments: serde_json::Value,
    output: String,
}

fn parse_registry_invocation(
    file: &str,
    arguments: &[String],
    info: Option<&harn_vm::tool_registry::ToolRegistryInfo>,
    tools: &[harn_vm::tool_registry::ToolCatalogEntry],
) -> Result<Option<RegistryInvocation>, String> {
    let root = command_tree(tools)?;
    let binary_name = info
        .map(|info| info.name.clone())
        .or_else(|| {
            Path::new(file)
                .file_stem()
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "harn-tool".to_string());
    let mut command = clap_command(binary_name.clone(), &root, tools)?;
    if let Some(info) = info {
        if let Some(version) = info.version.as_ref() {
            command = command.version(version.clone());
        }
        if let Some(description) = info.description.as_ref() {
            command = command.about(description.clone());
        }
    }
    let argv = std::iter::once(binary_name)
        .chain(arguments.iter().cloned())
        .collect::<Vec<_>>();
    let matches = match command.try_get_matches_from(argv) {
        Ok(matches) => matches,
        Err(error)
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) =>
        {
            error
                .print()
                .map_err(|print_error| print_error.to_string())?;
            return Ok(None);
        }
        Err(error) => return Err(error.to_string()),
    };
    let (tool_index, leaf) = selected_leaf(&matches, &root)?;
    let tool = &tools[tool_index];
    let mut input = read_base_input(leaf.get_one::<String>("__harn_input"))?;
    let object = input
        .as_object_mut()
        .ok_or_else(|| "--harn-input must resolve to a JSON object".to_string())?;
    let properties = tool
        .input_schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (name, schema) in &properties {
        if let Some(value) = leaf.get_one::<String>(name) {
            object.insert(name.clone(), coerce_argument(name, value, schema)?);
        }
    }
    let validator = jsonschema::draft202012::new(&tool.input_schema)
        .map_err(|error| format!("tool {:?} has an invalid input schema: {error}", tool.name))?;
    let violations = validator
        .iter_errors(&input)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if !violations.is_empty() {
        return Err(format!(
            "arguments for tool {:?} do not match its schema:\n  - {}",
            tool.name,
            violations.join("\n  - ")
        ));
    }
    Ok(Some(RegistryInvocation {
        tool_name: tool.name.clone(),
        arguments: input,
        output: leaf
            .get_one::<String>("__harn_output")
            .cloned()
            .unwrap_or_else(|| "json".to_string()),
    }))
}

fn command_tree(tools: &[harn_vm::tool_registry::ToolCatalogEntry]) -> Result<CommandNode, String> {
    let mut root = CommandNode::default();
    for (index, tool) in tools.iter().enumerate() {
        let mut node = &mut root;
        for part in &tool.cli.command {
            if node.tool_index.is_some() {
                return Err(format!(
                    "CLI command {:?} is both a tool and a parent command",
                    tool.cli.command
                ));
            }
            node = node.children.entry(part.clone()).or_default();
        }
        if !node.children.is_empty() {
            return Err(format!(
                "CLI command {:?} is both a parent command and a tool",
                tool.cli.command
            ));
        }
        node.tool_index = Some(index);
    }
    Ok(root)
}

fn clap_command(
    name: String,
    node: &CommandNode,
    tools: &[harn_vm::tool_registry::ToolCatalogEntry],
) -> Result<Command, String> {
    let mut command = Command::new(name)
        .subcommand_required(true)
        .arg_required_else_help(true);
    for (child_name, child) in &node.children {
        let mut subcommand = clap_command(child_name.clone(), child, tools)?;
        if let Some(index) = child.tool_index {
            let tool = &tools[index];
            subcommand = subcommand
                .about(tool.description.clone())
                .hide(tool.cli.hidden)
                .subcommand_required(false)
                .arg_required_else_help(false)
                .arg(
                    Arg::new("__harn_input")
                        .long("harn-input")
                        .value_name("JSON|@FILE|-")
                        .help("Base JSON object; individual flags override its properties"),
                )
                .arg(
                    Arg::new("__harn_output")
                        .long("harn-output")
                        .value_parser(["json", "pretty", "text"])
                        .default_value("json")
                        .help("Output encoding"),
                );
            let properties = tool
                .input_schema
                .get("properties")
                .and_then(serde_json::Value::as_object)
                .cloned()
                .unwrap_or_default();
            let mut long_names = BTreeSet::new();
            for (property_name, schema) in properties {
                let long_name = property_name.replace('_', "-");
                if long_name.is_empty()
                    || long_name.starts_with('-')
                    || !long_name
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric() || character == '-')
                {
                    return Err(format!(
                        "tool {:?} parameter {property_name:?} cannot be projected as a portable CLI flag",
                        tool.name
                    ));
                }
                if matches!(long_name.as_str(), "harn-input" | "harn-output")
                    || !long_names.insert(long_name.clone())
                {
                    return Err(format!(
                        "tool {:?} has parameters that collide at CLI flag --{long_name}",
                        tool.name
                    ));
                }
                let mut argument = Arg::new(property_name.clone())
                    .long(long_name)
                    .action(ArgAction::Set)
                    .value_name(schema_value_name(&schema));
                if let Some(description) = schema
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                {
                    argument = argument.help(description.to_string());
                }
                command_arg_schema_guard(&tool.name, &property_name, &schema)?;
                subcommand = subcommand.arg(argument);
            }
        }
        command = command.subcommand(subcommand);
    }
    Ok(command)
}

fn selected_leaf<'a>(
    matches: &'a ArgMatches,
    root: &CommandNode,
) -> Result<(usize, &'a ArgMatches), String> {
    let mut current_matches = matches;
    let mut current_node = root;
    while let Some((name, child_matches)) = current_matches.subcommand() {
        current_node = current_node
            .children
            .get(name)
            .ok_or_else(|| format!("generated parser selected unknown command {name:?}"))?;
        current_matches = child_matches;
    }
    current_node
        .tool_index
        .map(|index| (index, current_matches))
        .ok_or_else(|| "a leaf tool command is required".to_string())
}

fn read_base_input(source: Option<&String>) -> Result<serde_json::Value, String> {
    let Some(source) = source else {
        return Ok(serde_json::json!({}));
    };
    let text = if source == "-" {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|error| format!("failed to read --harn-input from stdin: {error}"))?;
        text
    } else if let Some(path) = source.strip_prefix('@') {
        fs::read_to_string(path)
            .map_err(|error| format!("failed to read --harn-input file {path:?}: {error}"))?
    } else {
        source.clone()
    };
    serde_json::from_str(&text).map_err(|error| format!("invalid --harn-input JSON: {error}"))
}

fn coerce_argument(
    name: &str,
    value: &str,
    schema: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    match schema_type(schema) {
        Some("string") => Ok(serde_json::Value::String(value.to_string())),
        Some("integer") => value
            .parse::<i64>()
            .map(serde_json::Value::from)
            .map_err(|_| format!("--{} expects an integer", name.replace('_', "-"))),
        Some("number") => value
            .parse::<f64>()
            .map(serde_json::Value::from)
            .map_err(|_| format!("--{} expects a number", name.replace('_', "-"))),
        Some("boolean") => match value {
            "true" => Ok(serde_json::Value::Bool(true)),
            "false" => Ok(serde_json::Value::Bool(false)),
            _ => Err(format!(
                "--{} expects true or false",
                name.replace('_', "-")
            )),
        },
        Some("object" | "array" | "null") | None => serde_json::from_str(value)
            .map_err(|error| format!("--{} expects JSON: {error}", name.replace('_', "-"))),
        Some(kind) => Err(format!(
            "--{} uses unsupported JSON Schema type {kind:?}",
            name.replace('_', "-")
        )),
    }
}

fn schema_type(schema: &serde_json::Value) -> Option<&str> {
    schema.get("type").and_then(serde_json::Value::as_str)
}

fn schema_value_name(schema: &serde_json::Value) -> &'static str {
    match schema_type(schema) {
        Some("integer") => "INT",
        Some("number") => "NUMBER",
        Some("boolean") => "BOOL",
        Some("object" | "array") | None => "JSON",
        _ => "VALUE",
    }
}

fn command_arg_schema_guard(
    tool_name: &str,
    property_name: &str,
    schema: &serde_json::Value,
) -> Result<(), String> {
    if schema.get("$ref").is_some() {
        return Err(format!(
            "tool {tool_name:?} parameter {property_name:?} contains an unresolved $ref"
        ));
    }
    if let Some(kind) = schema_type(schema) {
        if !matches!(
            kind,
            "string" | "integer" | "number" | "boolean" | "object" | "array" | "null"
        ) {
            return Err(format!(
                "tool {tool_name:?} parameter {property_name:?} uses unsupported JSON Schema type {kind:?}"
            ));
        }
    }
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
    let _version_env = ScopedEnvVar::set("HARN_TOOL_HARN_VERSION", env!("CARGO_PKG_VERSION"));
    let exit = dispatch::dispatch_to_embedded_script_with_sandbox(
        "scaffold/tool_new",
        Vec::new(),
        /* json_mode */ false,
        RunSandboxOptions::default().with_workspace_root(dest),
    )
    .await;
    if exit != 0 {
        return Err(format!("tool new scaffolder exited with code {exit}").into());
    }
    Ok(())
}

fn harn_identifier(name: &str) -> Result<String, PackageError> {
    harn_identifier_with_prefix(name, "tool")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn harn_identifier_normalizes_package_names() {
        assert_eq!(harn_identifier("acme-tool").unwrap(), "acme_tool");
        assert_eq!(harn_identifier("123-tool").unwrap(), "tool_123_tool");
    }

    fn catalog_tool(
        name: &str,
        command: &[&str],
        input_schema: serde_json::Value,
    ) -> harn_vm::tool_registry::ToolCatalogEntry {
        harn_vm::tool_registry::ToolCatalogEntry {
            name: name.to_string(),
            title: None,
            description: format!("Run {name}"),
            input_schema,
            output_schema: None,
            annotations: None,
            icons: None,
            execution: None,
            cli: harn_vm::tool_registry::ToolCliSpec {
                command: command.iter().map(|part| (*part).to_string()).collect(),
                hidden: false,
            },
            namespace: None,
            defer_loading: false,
            source: None,
            policy: None,
            meta: None,
        }
    }

    #[test]
    fn generated_cli_coerces_flags_and_validates_the_canonical_schema() {
        let tool = catalog_tool(
            "lookup_widget",
            &["widgets", "get"],
            serde_json::json!({
                "type": "object",
                "properties": {
                    "widget_id": {"type": "integer"},
                    "verbose": {"type": "boolean"}
                },
                "required": ["widget_id"],
                "additionalProperties": false
            }),
        );
        let invocation = parse_registry_invocation(
            "server.harn",
            &[
                "widgets".into(),
                "get".into(),
                "--widget-id".into(),
                "42".into(),
                "--verbose".into(),
                "false".into(),
                "--harn-output".into(),
                "pretty".into(),
            ],
            None,
            &[tool],
        )
        .unwrap()
        .unwrap();
        assert_eq!(invocation.tool_name, "lookup_widget");
        assert_eq!(
            invocation.arguments,
            serde_json::json!({"widget_id": 42, "verbose": false})
        );
        assert_eq!(invocation.output, "pretty");
    }

    #[test]
    fn generated_cli_rejects_missing_required_input() {
        let tool = catalog_tool(
            "lookup_widget",
            &["widgets", "get"],
            serde_json::json!({
                "type": "object",
                "properties": {"widget_id": {"type": "string"}},
                "required": ["widget_id"]
            }),
        );
        let error = parse_registry_invocation(
            "server.harn",
            &["widgets".into(), "get".into()],
            None,
            &[tool],
        )
        .unwrap_err();
        assert!(error.contains("widget_id"));
    }

    #[test]
    fn generated_cli_rejects_leaf_parent_ambiguity() {
        let error = command_tree(&[
            catalog_tool("widgets", &["widgets"], serde_json::json!({})),
            catalog_tool("get_widget", &["widgets", "get"], serde_json::json!({})),
        ])
        .err()
        .expect("ambiguous tree");
        assert!(error.contains("both a tool and a parent"));
    }

    #[test]
    fn generated_tool_projects_typed_harness_and_is_canonically_formatted() {
        let temp = tempfile::tempdir().expect("tempdir");
        let destination = temp.path().join("example-tool");
        let destination_arg = destination.display().to_string();
        let result = std::thread::Builder::new()
            .name("typed-tool-scaffold".to_string())
            .stack_size(crate::CLI_RUNTIME_STACK_SIZE)
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("runtime");
                runtime.block_on(async {
                    let _guard =
                        crate::tests::common::harn_state_lock::lock_harn_state_async().await;
                    run_new(&ToolNewArgs {
                        name: "example-tool".to_string(),
                        description: None,
                        dir: Some(destination_arg),
                        force: false,
                    })
                    .await
                })
            })
            .expect("scaffold thread")
            .join()
            .expect("scaffold thread completed");
        result.expect("tool scaffold");

        for relative in ["main.harn", "tests/test_tool.harn"] {
            let source = fs::read_to_string(destination.join(relative)).expect("generated source");
            assert!(source.contains("harness: Harness"), "{relative}");
            assert!(
                source.contains("agent_dispatch_tool_call(\n    harness.tools,"),
                "{relative}"
            );
            assert_generated_harn_is_formatted(destination.as_path(), relative);
        }
    }

    fn assert_generated_harn_is_formatted(root: &Path, relative: &str) {
        let source = fs::read_to_string(root.join(relative)).expect("generated source");
        let formatted = harn_fmt::format_source(&source).expect("format generated source");
        assert_eq!(source, formatted, "{relative} is not canonical");
    }

    // The force-overwrite behavior moved to a subprocess parity test in
    // `tests/scaffold_dispatch.rs`: the dispatched `.harn` impl runs the
    // full VM and would overflow the default `#[tokio::test]` thread
    // stack here. The subprocess inherits the workspace-level 16 MiB
    // runtime stack via `harn_cli::install_signal_shutdown_handler`.
}
