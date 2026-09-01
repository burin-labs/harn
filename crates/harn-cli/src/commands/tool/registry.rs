use std::fs;
use std::io::Read;
use std::path::Path;

use clap::{Arg, ArgAction, ArgMatches, Command, ValueHint};

use crate::cli::{
    ToolCompletionShell, ToolCompletionsArgs, ToolRunArgs, ToolSchemaArgs, ToolSchemaSurface,
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
    let catalog = harn_vm::tool_registry::tool_registry_catalog_for_audience(
        &loaded.registry,
        harn_vm::tool_registry::ToolAudience::Cli,
    )
    .map_err(|error| error.to_string())?;
    let prepared = harn_vm::tool_registry::PreparedToolCatalog::prepare(catalog)
        .map_err(|error| error.to_string())?;
    let tools = harn_vm::tool_registry::executable_tools_for_audience(
        &loaded.registry,
        harn_vm::tool_registry::ToolAudience::Cli,
    )
    .map_err(|error| error.to_string())?;
    let invocation = match parse_registry_invocation(&args.file, &args.arguments, &prepared)? {
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
    prepared
        .validate_output(&tool.catalog.name, &json)
        .map_err(|error| error.to_string())?;
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

/// Generate static completion from the same prepared tree used for parsing.
pub(crate) async fn print_registry_completions(
    args: &ToolCompletionsArgs,
) -> Result<(), ToolCommandError> {
    let loaded = crate::commands::run::load_file_tool_registry(&args.file)
        .await
        .map_err(|error| ToolCommandError {
            message: error.message,
            exit_code: error.exit_code,
        })?;
    if !loaded.diagnostics.is_empty() {
        eprint!("{}", loaded.diagnostics);
    }
    let catalog = harn_vm::tool_registry::tool_registry_catalog_for_audience(
        &loaded.registry,
        harn_vm::tool_registry::ToolAudience::Cli,
    )
    .map_err(|error| ToolCommandError::message(error.to_string()))?;
    let prepared = harn_vm::tool_registry::PreparedToolCatalog::prepare(catalog)
        .map_err(|error| ToolCommandError::message(error.to_string()))?;
    let binary_name = prepared
        .catalog()
        .info
        .as_ref()
        .map(|info| info.name.clone())
        .or_else(|| {
            Path::new(&args.file)
                .file_stem()
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "harn-tool".to_string());
    let mut command =
        prepared_clap_command(binary_name.clone(), &prepared).map_err(ToolCommandError::message)?;
    clap_complete::generate(
        completion_shell(args.shell),
        &mut command,
        binary_name,
        &mut std::io::stdout(),
    );
    Ok(())
}

fn completion_shell(shell: ToolCompletionShell) -> clap_complete::Shell {
    match shell {
        ToolCompletionShell::Bash => clap_complete::Shell::Bash,
        ToolCompletionShell::Zsh => clap_complete::Shell::Zsh,
        ToolCompletionShell::Fish => clap_complete::Shell::Fish,
        ToolCompletionShell::PowerShell => clap_complete::Shell::PowerShell,
    }
}

pub(crate) async fn print_registry_schema(args: &ToolSchemaArgs) -> Result<(), ToolCommandError> {
    let catalog = load_schema_catalog(args).await?;
    print_catalog(args, &catalog).map_err(ToolCommandError::message)
}

async fn load_schema_catalog(
    args: &ToolSchemaArgs,
) -> Result<harn_vm::tool_registry::ToolCatalog, ToolCommandError> {
    let catalog = match args.surface {
        ToolSchemaSurface::Script => {
            let loaded = crate::commands::run::load_file_tool_registry(&args.file)
                .await
                .map_err(|error| ToolCommandError {
                    message: error.message,
                    exit_code: error.exit_code,
                })?;
            if !loaded.diagnostics.is_empty() {
                eprint!("{}", loaded.diagnostics);
            }
            harn_vm::tool_registry::tool_registry_catalog_for_audience(
                &loaded.registry,
                harn_vm::tool_registry::ToolAudience::Catalog,
            )
            .map_err(|error| ToolCommandError::message(error.to_string()))?
        }
        ToolSchemaSurface::Exports => {
            let catalog = harn_serve::ExportCatalog::from_path(Path::new(&args.file))
                .map_err(|error| ToolCommandError::message(error.message()))?;
            harn_serve::emit_export_diagnostics(catalog.diagnostics());
            catalog
                .tool_catalog()
                .map_err(|error| ToolCommandError::message(error.message()))?
        }
    };
    Ok(catalog)
}

fn print_catalog(
    args: &ToolSchemaArgs,
    catalog: &harn_vm::tool_registry::ToolCatalog,
) -> Result<(), String> {
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

#[derive(Debug)]
struct RegistryInvocation {
    tool_name: String,
    arguments: serde_json::Value,
    output: String,
}

fn parse_registry_invocation(
    file: &str,
    arguments: &[String],
    prepared: &harn_vm::tool_registry::PreparedToolCatalog,
) -> Result<Option<RegistryInvocation>, String> {
    let info = prepared.catalog().info.as_ref();
    let binary_name = info
        .map(|info| info.name.clone())
        .or_else(|| {
            Path::new(file)
                .file_stem()
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "harn-tool".to_string());
    let command = prepared_clap_command(binary_name.clone(), prepared)?;
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
    let (leaf_command, leaf) = selected_prepared_leaf(&matches, prepared.cli_tree())?;
    let tool_name = leaf_command
        .tool_name()
        .ok_or_else(|| "a leaf tool command is required".to_string())?;
    let tool = prepared
        .entry(tool_name)
        .ok_or_else(|| format!("generated parser selected unknown tool {tool_name:?}"))?;
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
    for argument in leaf_command.arguments() {
        let name = argument.property();
        let schema = properties.get(name).ok_or_else(|| {
            format!("prepared CLI argument {name:?} is absent from the input schema")
        })?;
        if argument.repeatable() {
            if let Some(values) = leaf.get_many::<String>(name) {
                let item_schema = schema
                    .get("items")
                    .expect("prepared repeatable arguments have an items schema");
                let values = values
                    .map(|value| coerce_argument(name, value, item_schema))
                    .collect::<Result<Vec<_>, _>>()?;
                object.insert(name.to_string(), serde_json::Value::Array(values));
            }
        } else if let Some(value) = leaf.get_one::<String>(name) {
            object.insert(name.to_string(), coerce_argument(name, value, schema)?);
        }
    }
    prepared
        .validate_input(&tool.name, &input)
        .map_err(|error| error.to_string())?;
    Ok(Some(RegistryInvocation {
        tool_name: tool.name.clone(),
        arguments: input,
        output: if leaf
            .try_get_one::<bool>("__harn_json")
            .ok()
            .flatten()
            .copied()
            .unwrap_or(false)
        {
            "json".to_string()
        } else {
            leaf.get_one::<String>("__harn_output")
                .cloned()
                .unwrap_or_else(|| "json".to_string())
        },
    }))
}

fn prepared_clap_command(
    name: String,
    prepared: &harn_vm::tool_registry::PreparedToolCatalog,
) -> Result<Command, String> {
    let mut command = Command::new(name)
        .subcommand_required(true)
        .arg_required_else_help(true);
    if let Some(info) = prepared.catalog().info.as_ref() {
        if let Some(version) = info.version.as_ref() {
            command = command.version(version.clone());
        }
        if let Some(description) = info.description.as_ref() {
            command = command.about(description.clone());
        }
    }
    add_prepared_subcommands(command, prepared.cli_tree().commands(), prepared)
}

fn add_prepared_subcommands(
    mut command: Command,
    children: &[harn_vm::tool_registry::PreparedCliCommand],
    prepared: &harn_vm::tool_registry::PreparedToolCatalog,
) -> Result<Command, String> {
    for child in children {
        let mut subcommand = Command::new(child.name().to_string())
            .visible_aliases(child.aliases().iter().cloned())
            .hide(child.hidden());
        if let Some(order) = child.display_order() {
            subcommand = subcommand.display_order(order as usize);
        }
        match (child.title(), child.description()) {
            (Some(title), Some(description)) => {
                subcommand = subcommand
                    .about(title.to_string())
                    .long_about(description.to_string());
            }
            (Some(title), None) => subcommand = subcommand.about(title.to_string()),
            (None, Some(description)) => {
                subcommand = subcommand.about(description.to_string());
            }
            (None, None) => {}
        }
        if child.tool_name().is_some() {
            subcommand = add_prepared_leaf_arguments(subcommand, child, prepared)?;
        } else {
            subcommand = subcommand
                .subcommand_required(true)
                .arg_required_else_help(true);
        }
        subcommand = add_prepared_subcommands(subcommand, child.children(), prepared)?;
        command = command.subcommand(subcommand);
    }
    Ok(command)
}

fn add_prepared_leaf_arguments(
    mut command: Command,
    leaf: &harn_vm::tool_registry::PreparedCliCommand,
    prepared: &harn_vm::tool_registry::PreparedToolCatalog,
) -> Result<Command, String> {
    let tool_name = leaf.tool_name().expect("prepared CLI leaf names one tool");
    let tool = prepared
        .entry(tool_name)
        .expect("prepared CLI leaf resolves to its catalog entry");
    let properties = tool
        .input_schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let has_json_input = leaf.arguments().iter().any(|argument| {
        argument.long() == Some("json") || argument.aliases().iter().any(|alias| alias == "json")
    });
    command = command
        .subcommand_required(false)
        .arg_required_else_help(false)
        .arg(
            Arg::new("__harn_input")
                .long("harn-input")
                .value_name("JSON|@FILE|-")
                .help("Base JSON object; individual arguments override its properties"),
        )
        .arg(
            Arg::new("__harn_output")
                .long("harn-output")
                .value_parser(["json", "pretty", "text"])
                .default_value("json")
                .help("Output encoding"),
        );
    if !has_json_input {
        command = command.arg(
            Arg::new("__harn_json")
                .long("json")
                .action(ArgAction::SetTrue)
                .conflicts_with("__harn_output")
                .help("Emit compact JSON (alias for --harn-output json)"),
        );
    }
    for projection in leaf.arguments() {
        let schema = properties.get(projection.property()).ok_or_else(|| {
            format!(
                "tool {tool_name:?} prepared CLI argument {:?} is absent from its input schema",
                projection.property()
            )
        })?;
        let value_schema = if projection.repeatable() {
            schema
                .get("items")
                .expect("prepared repeatable argument has items")
        } else {
            schema
        };
        command_arg_schema_guard(tool_name, projection.property(), value_schema)?;
        let mut argument = Arg::new(projection.property().to_string())
            .action(if projection.repeatable() {
                ArgAction::Append
            } else {
                ArgAction::Set
            })
            .value_name(projection.value_name().to_string());
        if let Some(position) = projection.position() {
            argument = argument.index((position + 1) as usize);
        } else if let Some(long) = projection.long() {
            argument = argument.long(long.to_string());
        }
        if let Some(short) = projection.short() {
            argument = argument.short(short);
        }
        if !projection.aliases().is_empty() {
            argument = argument.visible_aliases(projection.aliases().iter().cloned());
        }
        if let Some(help) = projection.help() {
            argument = argument.help(help.to_string());
        }
        if let Some(order) = projection.display_order() {
            argument = argument.display_order(order as usize);
        }
        if let Some(group) = projection.help_group() {
            argument = argument.help_heading(group.to_string());
        }
        if let Some(hint) = projection.value_hint() {
            argument = argument.value_hint(clap_value_hint(hint));
        }
        if let Some(values) = static_string_enum(value_schema) {
            argument = argument.value_parser(values);
        }
        command = command.arg(argument);
    }
    Ok(command)
}

fn clap_value_hint(hint: harn_vm::tool_registry::ToolCliValueHint) -> ValueHint {
    match hint {
        harn_vm::tool_registry::ToolCliValueHint::File => ValueHint::FilePath,
        harn_vm::tool_registry::ToolCliValueHint::Directory => ValueHint::DirPath,
        harn_vm::tool_registry::ToolCliValueHint::Path => ValueHint::AnyPath,
        harn_vm::tool_registry::ToolCliValueHint::Url => ValueHint::Url,
        harn_vm::tool_registry::ToolCliValueHint::Email => ValueHint::EmailAddress,
        harn_vm::tool_registry::ToolCliValueHint::Hostname => ValueHint::Hostname,
        harn_vm::tool_registry::ToolCliValueHint::Command => ValueHint::CommandName,
    }
}

fn static_string_enum(schema: &serde_json::Value) -> Option<Vec<String>> {
    schema
        .get("enum")?
        .as_array()?
        .iter()
        .map(|value| value.as_str().map(ToOwned::to_owned))
        .collect()
}

fn selected_prepared_leaf<'a>(
    matches: &'a ArgMatches,
    tree: &'a harn_vm::tool_registry::PreparedCliTree,
) -> Result<
    (
        &'a harn_vm::tool_registry::PreparedCliCommand,
        &'a ArgMatches,
    ),
    String,
> {
    let mut current_matches = matches;
    let mut children = tree.commands();
    let mut selected = None;
    while let Some((name, child_matches)) = current_matches.subcommand() {
        let child = children
            .iter()
            .find(|child| child.name() == name || child.aliases().iter().any(|alias| alias == name))
            .ok_or_else(|| format!("generated parser selected unknown command {name:?}"))?;
        selected = Some(child);
        children = child.children();
        current_matches = child_matches;
    }
    selected
        .map(|command| (command, current_matches))
        .filter(|(command, _)| command.tool_name().is_some())
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn catalog_tool(
        name: &str,
        command: &[&str],
        input_schema: serde_json::Value,
    ) -> harn_vm::tool_registry::ToolCatalogEntry {
        harn_vm::tool_registry::ToolCatalogEntry {
            name: name.to_string(),
            title: None,
            description: Some(format!("Run {name}")),
            input_schema,
            output_schema: None,
            annotations: None,
            icons: None,
            execution: None,
            governance: harn_vm::tool_registry::ToolGovernance::default(),
            cli: harn_vm::tool_registry::ToolCliSpec {
                command: command.iter().map(|part| (*part).to_string()).collect(),
                hidden: false,
                arguments: BTreeMap::new(),
            },
            namespace: None,
            defer_loading: false,
            source: None,
            policy: None,
            meta: None,
        }
    }

    fn test_catalog(
        tools: Vec<harn_vm::tool_registry::ToolCatalogEntry>,
    ) -> harn_vm::tool_registry::ToolCatalog {
        harn_vm::tool_registry::ToolCatalog {
            schema_version: harn_vm::tool_registry::ToolCatalogSchemaVersion::V1,
            info: None,
            cli: None,
            tools,
            components: None,
        }
    }

    fn prepared_catalog(
        tools: Vec<harn_vm::tool_registry::ToolCatalogEntry>,
    ) -> harn_vm::tool_registry::PreparedToolCatalog {
        harn_vm::tool_registry::PreparedToolCatalog::prepare(test_catalog(tools))
            .expect("prepare test catalog")
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
        let prepared = prepared_catalog(vec![tool]);
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
            &prepared,
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
    fn generated_cli_accepts_explicit_json_output_alias() {
        let tool = catalog_tool(
            "lookup_widget",
            &["widgets", "get"],
            serde_json::json!({"type": "object", "properties": {}}),
        );
        let prepared = prepared_catalog(vec![tool]);
        let invocation = parse_registry_invocation(
            "server.harn",
            &["widgets".into(), "get".into(), "--json".into()],
            &prepared,
        )
        .unwrap()
        .unwrap();

        assert_eq!(invocation.output, "json");
    }

    #[test]
    fn generated_cli_preserves_a_json_named_tool_input() {
        let tool = catalog_tool(
            "submit_payload",
            &["payloads", "submit"],
            serde_json::json!({
                "type": "object",
                "properties": {"json": {"type": "string"}},
                "required": ["json"]
            }),
        );
        let prepared = prepared_catalog(vec![tool]);
        let invocation = parse_registry_invocation(
            "server.harn",
            &[
                "payloads".into(),
                "submit".into(),
                "--json".into(),
                "raw-payload".into(),
            ],
            &prepared,
        )
        .unwrap()
        .unwrap();

        assert_eq!(invocation.arguments["json"], "raw-payload");
        assert_eq!(invocation.output, "json");
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
        let prepared = prepared_catalog(vec![tool]);
        let error =
            parse_registry_invocation("server.harn", &["widgets".into(), "get".into()], &prepared)
                .unwrap_err();
        assert!(error.contains("widget_id"));
    }

    #[test]
    fn generated_cli_uses_prepared_parent_and_argument_projections() {
        use harn_vm::tool_registry::{
            ToolCliArgumentSpec, ToolCliCommandSpec, ToolCliTreeSpec, ToolCliValueHint,
        };

        let mut tool = catalog_tool(
            "create_widget",
            &["widgets", "create"],
            serde_json::json!({
                "type": "object",
                "properties": {
                    "widget_id": {"type": "integer", "description": "Widget identifier"},
                    "tags": {"type": "array", "items": {"type": "string"}},
                    "mode": {"type": "string", "enum": ["safe", "fast"]}
                },
                "required": ["widget_id"],
                "additionalProperties": false
            }),
        );
        tool.cli.arguments = BTreeMap::from([
            (
                "widget_id".to_string(),
                ToolCliArgumentSpec {
                    position: Some(0),
                    value_name: Some("WIDGET".to_string()),
                    ..ToolCliArgumentSpec::default()
                },
            ),
            (
                "tags".to_string(),
                ToolCliArgumentSpec {
                    long: Some("tag".to_string()),
                    short: Some('t'),
                    aliases: vec!["label".to_string()],
                    value_name: Some("TAG".to_string()),
                    value_hint: Some(ToolCliValueHint::File),
                    repeatable: true,
                    display_order: Some(2),
                    help_group: Some("Selection".to_string()),
                    ..ToolCliArgumentSpec::default()
                },
            ),
        ]);
        let mut catalog = test_catalog(vec![tool]);
        catalog.info = Some(harn_vm::tool_registry::ToolRegistryInfo {
            name: "widgetctl".to_string(),
            version: None,
            description: None,
        });
        catalog.cli = Some(ToolCliTreeSpec {
            commands: vec![ToolCliCommandSpec {
                command: vec!["widgets".to_string()],
                title: Some("Manage widgets".to_string()),
                description: Some("Create and inspect durable widgets".to_string()),
                aliases: vec!["w".to_string()],
                hidden: false,
                display_order: Some(1),
            }],
        });
        let prepared = harn_vm::tool_registry::PreparedToolCatalog::prepare(catalog)
            .expect("prepare projected CLI");

        let invocation = parse_registry_invocation(
            "server.harn",
            &[
                "w".into(),
                "create".into(),
                "42".into(),
                "--tag".into(),
                "blue".into(),
                "--label".into(),
                "green".into(),
                "--mode".into(),
                "safe".into(),
            ],
            &prepared,
        )
        .expect("parse projected CLI")
        .expect("invocation");
        assert_eq!(
            invocation.arguments,
            serde_json::json!({
                "widget_id": 42,
                "tags": ["blue", "green"],
                "mode": "safe"
            })
        );

        let help = prepared_clap_command("widgetctl".to_string(), &prepared)
            .expect("project command")
            .render_long_help()
            .to_string();
        assert!(help.contains("Manage widgets"), "{help}");
        assert!(help.contains("[alias: w]"), "{help}");

        for shell in [
            clap_complete::Shell::Bash,
            clap_complete::Shell::Zsh,
            clap_complete::Shell::Fish,
            clap_complete::Shell::PowerShell,
        ] {
            let mut command =
                prepared_clap_command("widgetctl".to_string(), &prepared).expect("command tree");
            let mut output = Vec::new();
            clap_complete::generate(shell, &mut command, "widgetctl", &mut output);
            let script = String::from_utf8(output).expect("completion script is UTF-8");
            assert!(script.contains("widgets"), "{shell:?}: {script}");
            assert!(script.contains("create"), "{shell:?}: {script}");
            assert!(script.contains("tag"), "{shell:?}: {script}");
        }
    }

    #[test]
    fn generated_cli_rejects_leaf_parent_ambiguity() {
        let error = harn_vm::tool_registry::PreparedToolCatalog::prepare(test_catalog(vec![
            catalog_tool(
                "widgets",
                &["widgets"],
                serde_json::json!({"type": "object", "properties": {}}),
            ),
            catalog_tool(
                "get_widget",
                &["widgets", "get"],
                serde_json::json!({"type": "object", "properties": {}}),
            ),
        ]))
        .expect_err("ambiguous tree");
        assert!(error.to_string().contains("both a tool and a parent"));
    }

    #[test]
    fn generated_cli_tree_shares_the_portable_component_contract() {
        harn_vm::tool_registry::PreparedToolCatalog::prepare(test_catalog(vec![catalog_tool(
            "nested",
            &["inspect", "inspect"],
            serde_json::json!({"type": "object", "properties": {}}),
        )]))
        .expect("repeated components are a valid nested command path");

        for invalid in [["-inspect"], ["inspect me"]] {
            let error = harn_vm::tool_registry::PreparedToolCatalog::prepare(test_catalog(vec![
                catalog_tool(
                    "invalid",
                    &invalid,
                    serde_json::json!({"type": "object", "properties": {}}),
                ),
            ]))
            .expect_err("invalid component");
            assert!(error
                .to_string()
                .contains("must match ^[A-Za-z0-9_][A-Za-z0-9_-]*$"));
        }
    }

    #[tokio::test]
    async fn export_schema_surface_is_offline_even_when_main_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("offline.harn");
        std::fs::write(
            &script,
            r#"
fn main() { panic("must not execute") }
pub fn inspect(input: {id: string}) -> {id: string} { return input }
"#,
        )
        .expect("write script");
        let args = ToolSchemaArgs {
            file: script.display().to_string(),
            surface: ToolSchemaSurface::Exports,
            pretty: false,
        };

        let catalog = load_schema_catalog(&args)
            .await
            .expect("offline export catalog");
        assert_eq!(catalog.tools.len(), 1);
        assert_eq!(catalog.tools[0].name, "inspect");
        assert_eq!(
            catalog.tools[0].input_schema["properties"]["input"]["properties"]["id"]["type"],
            "string"
        );
    }
}
