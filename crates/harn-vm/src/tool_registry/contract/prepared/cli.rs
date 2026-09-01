use std::collections::BTreeMap;

use serde_json::Value as JsonValue;

use super::PreparedToolCatalogError;
use crate::tool_registry::{
    is_valid_cli_command_component, ToolAudience, ToolCatalog, ToolCliArgumentSpec,
    ToolCliCommandSpec, ToolCliValueHint,
};

const RESERVED_LONG_NAMES: [&str; 4] = ["harn-input", "harn-output", "help", "version"];
const RESERVED_SHORT_NAMES: [char; 2] = ['h', 'V'];
const RESERVED_COMMAND_NAMES: [&str; 1] = ["help"];

/// One normalized token-to-property mapping in a prepared CLI tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedCliArgument {
    property: String,
    long: Option<String>,
    short: Option<char>,
    aliases: Vec<String>,
    position: Option<u32>,
    value_name: String,
    help: Option<String>,
    value_hint: Option<ToolCliValueHint>,
    repeatable: bool,
    display_order: Option<u32>,
    help_group: Option<String>,
}

impl PreparedCliArgument {
    pub fn property(&self) -> &str {
        &self.property
    }

    pub fn long(&self) -> Option<&str> {
        self.long.as_deref()
    }

    pub fn short(&self) -> Option<char> {
        self.short
    }

    pub fn aliases(&self) -> &[String] {
        &self.aliases
    }

    pub fn position(&self) -> Option<u32> {
        self.position
    }

    pub fn value_name(&self) -> &str {
        &self.value_name
    }

    pub fn help(&self) -> Option<&str> {
        self.help.as_deref()
    }

    pub fn value_hint(&self) -> Option<ToolCliValueHint> {
        self.value_hint
    }

    pub fn repeatable(&self) -> bool {
        self.repeatable
    }

    pub fn display_order(&self) -> Option<u32> {
        self.display_order
    }

    pub fn help_group(&self) -> Option<&str> {
        self.help_group.as_deref()
    }
}

/// One node in the framework-independent prepared CLI tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedCliCommand {
    name: String,
    path: Vec<String>,
    title: Option<String>,
    description: Option<String>,
    aliases: Vec<String>,
    hidden: bool,
    display_order: Option<u32>,
    tool_name: Option<String>,
    arguments: Vec<PreparedCliArgument>,
    children: Vec<PreparedCliCommand>,
}

impl PreparedCliCommand {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> &[String] {
        &self.path
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    pub fn aliases(&self) -> &[String] {
        &self.aliases
    }

    pub fn hidden(&self) -> bool {
        self.hidden
    }

    pub fn display_order(&self) -> Option<u32> {
        self.display_order
    }

    pub fn tool_name(&self) -> Option<&str> {
        self.tool_name.as_deref()
    }

    pub fn arguments(&self) -> &[PreparedCliArgument] {
        &self.arguments
    }

    pub fn children(&self) -> &[PreparedCliCommand] {
        &self.children
    }
}

/// Immutable portable command tree derived once from a prepared catalog.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PreparedCliTree {
    commands: Vec<PreparedCliCommand>,
}

impl PreparedCliTree {
    pub(super) fn prepare(catalog: &ToolCatalog) -> Result<Self, PreparedToolCatalogError> {
        let metadata = catalog
            .cli
            .as_ref()
            .into_iter()
            .flat_map(|cli| cli.commands.iter())
            .map(|command| (command.command.clone(), command))
            .collect::<BTreeMap<_, _>>();
        if metadata.len() != catalog.cli.as_ref().map_or(0, |cli| cli.commands.len()) {
            return Err(error("duplicate registry-level CLI command metadata path"));
        }

        let mut root = MutableCommand::default();
        for tool in &catalog.tools {
            let arguments = prepare_arguments(tool)?;
            if !tool.governance.allows(ToolAudience::Cli) {
                continue;
            }
            let mut node = &mut root;
            for (index, part) in tool.cli.command.iter().enumerate() {
                let path = &tool.cli.command[..=index];
                node = node
                    .children
                    .entry(part.clone())
                    .or_insert_with(|| MutableCommand::new(path.to_vec()));
            }
            node.tool_name = Some(tool.name.clone());
            node.hidden |= tool.cli.hidden;
            node.description = tool.description.clone();
            node.title = tool.title.clone();
            node.arguments = arguments;
        }

        for (path, command) in &metadata {
            if path.is_empty()
                || path
                    .iter()
                    .any(|part| !is_valid_cli_command_component(part))
            {
                return Err(error(format!(
                    "registry-level CLI command path {:?} is invalid",
                    path.join(" ")
                )));
            }
            let node = root.find_mut(path).ok_or_else(|| {
                error(format!(
                    "registry-level CLI metadata path {:?} does not name a command",
                    path.join(" ")
                ))
            })?;
            if node.tool_name.is_some() {
                return Err(error(format!(
                    "registry-level CLI metadata path {:?} names a tool rather than a parent command",
                    path.join(" ")
                )));
            }
            node.apply_metadata(command);
        }

        root.validate_sibling_names()?;
        Ok(Self {
            commands: root.finish_children(),
        })
    }

    pub fn commands(&self) -> &[PreparedCliCommand] {
        &self.commands
    }

    pub fn find(&self, path: &[String]) -> Option<&PreparedCliCommand> {
        let (first, rest) = path.split_first()?;
        self.commands
            .iter()
            .find(|command| command.name == *first || command.aliases.contains(first))?
            .find(rest)
    }
}

#[derive(Default)]
struct MutableCommand {
    path: Vec<String>,
    title: Option<String>,
    description: Option<String>,
    aliases: Vec<String>,
    hidden: bool,
    display_order: Option<u32>,
    tool_name: Option<String>,
    arguments: Vec<PreparedCliArgument>,
    children: BTreeMap<String, MutableCommand>,
}

impl MutableCommand {
    fn new(path: Vec<String>) -> Self {
        Self {
            path,
            ..Self::default()
        }
    }

    fn find_mut(&mut self, path: &[String]) -> Option<&mut Self> {
        let (first, rest) = path.split_first()?;
        let child = self.children.get_mut(first)?;
        if rest.is_empty() {
            Some(child)
        } else {
            child.find_mut(rest)
        }
    }

    fn apply_metadata(&mut self, metadata: &ToolCliCommandSpec) {
        self.title = metadata.title.clone();
        self.description = metadata.description.clone();
        self.aliases = metadata.aliases.clone();
        self.hidden = metadata.hidden;
        self.display_order = metadata.display_order;
    }

    fn validate_sibling_names(&self) -> Result<(), PreparedToolCatalogError> {
        let mut owners = BTreeMap::<String, String>::new();
        for (name, child) in &self.children {
            validate_optional_text(child.title.as_deref(), "CLI command title")?;
            validate_optional_text(child.description.as_deref(), "CLI command description")?;
            for spelling in std::iter::once(name).chain(child.aliases.iter()) {
                if !is_valid_cli_command_component(spelling) {
                    return Err(error(format!(
                        "CLI command spelling {spelling:?} is not a portable command component"
                    )));
                }
                if RESERVED_COMMAND_NAMES.contains(&spelling.as_str()) {
                    return Err(error(format!(
                        "CLI command spelling {spelling:?} is reserved by the generated command framework"
                    )));
                }
                if let Some(owner) = owners.insert(spelling.clone(), name.clone()) {
                    return Err(error(format!(
                        "CLI command spelling {spelling:?} is shared by sibling commands {owner:?} and {name:?}"
                    )));
                }
            }
            child.validate_sibling_names()?;
        }
        Ok(())
    }

    fn finish_children(self) -> Vec<PreparedCliCommand> {
        self.children
            .into_iter()
            .map(|(name, child)| child.finish(name))
            .collect()
    }

    fn finish(self, name: String) -> PreparedCliCommand {
        let Self {
            path,
            title,
            description,
            aliases,
            hidden,
            display_order,
            tool_name,
            arguments,
            children,
        } = self;
        PreparedCliCommand {
            name,
            path,
            title,
            description,
            aliases,
            hidden,
            display_order,
            tool_name,
            arguments,
            children: children
                .into_iter()
                .map(|(name, child)| child.finish(name))
                .collect(),
        }
    }
}

impl PreparedCliCommand {
    fn find(&self, path: &[String]) -> Option<&Self> {
        let Some((first, rest)) = path.split_first() else {
            return Some(self);
        };
        self.children
            .iter()
            .find(|command| command.name == *first || command.aliases.contains(first))?
            .find(rest)
    }
}

fn prepare_arguments(
    tool: &crate::tool_registry::ToolCatalogEntry,
) -> Result<Vec<PreparedCliArgument>, PreparedToolCatalogError> {
    let properties = tool
        .input_schema
        .get("properties")
        .and_then(JsonValue::as_object)
        .cloned()
        .unwrap_or_default();
    for property in tool.cli.arguments.keys() {
        if !properties.contains_key(property) {
            return Err(error(format!(
                "tool {:?} CLI argument metadata names unknown input property {property:?}",
                tool.name
            )));
        }
    }

    let mut long_names = BTreeMap::<String, String>::new();
    let mut short_names = BTreeMap::<char, String>::new();
    let mut positions = BTreeMap::<u32, String>::new();
    let mut arguments = Vec::with_capacity(properties.len());
    for (property, schema) in properties {
        let projection = tool
            .cli
            .arguments
            .get(&property)
            .cloned()
            .unwrap_or_default();
        let argument = prepare_argument(&tool.name, property, &schema, projection)?;
        if let Some(position) = argument.position {
            if let Some(owner) = positions.insert(position, argument.property.clone()) {
                return Err(error(format!(
                    "tool {:?} CLI position {position} is shared by properties {owner:?} and {:?}",
                    tool.name, argument.property
                )));
            }
        }
        for spelling in argument.long.iter().chain(argument.aliases.iter()) {
            if RESERVED_LONG_NAMES.contains(&spelling.as_str()) {
                return Err(error(format!(
                    "tool {:?} CLI argument {:?} collides with reserved --{spelling}",
                    tool.name, argument.property
                )));
            }
            if let Some(owner) = long_names.insert(spelling.clone(), argument.property.clone()) {
                return Err(error(format!(
                    "tool {:?} CLI spelling --{spelling} is shared by properties {owner:?} and {:?}",
                    tool.name, argument.property
                )));
            }
        }
        if let Some(short) = argument.short {
            if RESERVED_SHORT_NAMES.contains(&short) {
                return Err(error(format!(
                    "tool {:?} CLI argument {:?} collides with reserved -{short}",
                    tool.name, argument.property
                )));
            }
            if let Some(owner) = short_names.insert(short, argument.property.clone()) {
                return Err(error(format!(
                    "tool {:?} CLI spelling -{short} is shared by properties {owner:?} and {:?}",
                    tool.name, argument.property
                )));
            }
        }
        arguments.push(argument);
    }

    for (expected, position) in positions.keys().copied().enumerate() {
        if position != expected as u32 {
            return Err(error(format!(
                "tool {:?} CLI positional indexes must be dense from 0; expected {expected}, found {position}",
                tool.name
            )));
        }
    }
    arguments.sort_by_key(|argument| {
        (
            argument.position.unwrap_or(u32::MAX),
            argument.display_order.unwrap_or(u32::MAX),
            argument.property.clone(),
        )
    });
    Ok(arguments)
}

fn prepare_argument(
    tool: &str,
    property: String,
    schema: &JsonValue,
    projection: ToolCliArgumentSpec,
) -> Result<PreparedCliArgument, PreparedToolCatalogError> {
    if projection.position.is_some()
        && (projection.long.is_some()
            || projection.short.is_some()
            || !projection.aliases.is_empty())
    {
        return Err(error(format!(
            "tool {tool:?} property {property:?} cannot be both positional and named"
        )));
    }
    if projection
        .short
        .is_some_and(|short| !short.is_ascii_alphanumeric())
    {
        return Err(error(format!(
            "tool {tool:?} property {property:?} short spelling must be one ASCII letter or digit"
        )));
    }
    validate_optional_text(projection.value_name.as_deref(), "CLI argument value_name")?;
    validate_optional_text(projection.help.as_deref(), "CLI argument help")?;
    validate_optional_text(projection.help_group.as_deref(), "CLI argument help_group")?;
    let is_array = schema.get("type").and_then(JsonValue::as_str) == Some("array");
    if projection.repeatable && !is_array {
        return Err(error(format!(
            "tool {tool:?} property {property:?} can be repeatable only when its schema type is array"
        )));
    }
    if projection.repeatable
        && !schema
            .get("items")
            .is_some_and(|items| items.is_object() || items.is_boolean())
    {
        return Err(error(format!(
            "tool {tool:?} property {property:?} needs one JSON Schema 'items' schema for repeatable CLI tokens"
        )));
    }
    let long = if projection.position.is_some() {
        None
    } else {
        Some(
            projection
                .long
                .unwrap_or_else(|| property.replace('_', "-")),
        )
    };
    for spelling in long.iter().chain(projection.aliases.iter()) {
        if !valid_long_name(spelling) {
            return Err(error(format!(
                "tool {tool:?} property {property:?} has invalid CLI spelling --{spelling}"
            )));
        }
    }
    let value_name = projection.value_name.unwrap_or_else(|| {
        schema
            .get("title")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| property.to_ascii_uppercase())
    });
    let help = projection.help.or_else(|| {
        schema
            .get("description")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned)
    });
    Ok(PreparedCliArgument {
        property,
        long,
        short: projection.short,
        aliases: projection.aliases,
        position: projection.position,
        value_name,
        help,
        value_hint: projection.value_hint,
        repeatable: projection.repeatable,
        display_order: projection.display_order,
        help_group: projection.help_group,
    })
}

fn valid_long_name(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '-')
}

fn validate_optional_text(
    value: Option<&str>,
    field: &str,
) -> Result<(), PreparedToolCatalogError> {
    if value.is_some_and(|value| value.trim().is_empty()) {
        return Err(error(format!("{field} must not be empty")));
    }
    Ok(())
}

fn error(message: impl Into<String>) -> PreparedToolCatalogError {
    PreparedToolCatalogError::new(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_registry::{
        ToolCatalogEntry, ToolCatalogSchemaVersion, ToolCliSpec, ToolCliTreeSpec, ToolGovernance,
    };
    use serde_json::json;

    fn tool() -> ToolCatalogEntry {
        ToolCatalogEntry {
            name: "create_widget".to_string(),
            title: Some("Create widget".to_string()),
            description: Some("Create one durable widget".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "widget_id": {"type": "integer", "description": "Widget identifier"},
                    "tags": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["widget_id"],
                "additionalProperties": false
            }),
            output_schema: None,
            annotations: None,
            icons: None,
            execution: None,
            governance: ToolGovernance::default(),
            cli: ToolCliSpec {
                command: vec!["widgets".to_string(), "create".to_string()],
                hidden: false,
                arguments: BTreeMap::from([
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
                            repeatable: true,
                            ..ToolCliArgumentSpec::default()
                        },
                    ),
                ]),
            },
            namespace: None,
            defer_loading: false,
            source: None,
            policy: None,
            meta: None,
        }
    }

    fn catalog(tool: ToolCatalogEntry) -> ToolCatalog {
        ToolCatalog {
            schema_version: ToolCatalogSchemaVersion::V1,
            info: None,
            cli: Some(ToolCliTreeSpec {
                commands: vec![ToolCliCommandSpec {
                    command: vec!["widgets".to_string()],
                    title: Some("Manage widgets".to_string()),
                    description: None,
                    aliases: vec!["w".to_string()],
                    hidden: false,
                    display_order: Some(1),
                }],
            }),
            tools: vec![tool],
            components: None,
        }
    }

    #[test]
    fn normalizes_parent_metadata_and_argument_tokens_once() {
        let prepared = PreparedCliTree::prepare(&catalog(tool())).expect("prepare CLI tree");
        let parent = &prepared.commands()[0];
        assert_eq!(parent.path(), &["widgets"]);
        assert_eq!(parent.title(), Some("Manage widgets"));
        assert_eq!(parent.aliases(), &["w"]);
        let leaf = &parent.children()[0];
        assert_eq!(leaf.tool_name(), Some("create_widget"));
        assert_eq!(leaf.arguments()[0].property(), "widget_id");
        assert_eq!(leaf.arguments()[0].position(), Some(0));
        assert_eq!(leaf.arguments()[1].long(), Some("tag"));
        assert!(leaf.arguments()[1].repeatable());
    }

    #[test]
    fn rejects_unknown_sparse_duplicate_reserved_and_schema_weakening_metadata() {
        let mut unknown = tool();
        unknown
            .cli
            .arguments
            .insert("missing".to_string(), ToolCliArgumentSpec::default());
        assert!(PreparedCliTree::prepare(&catalog(unknown))
            .unwrap_err()
            .to_string()
            .contains("unknown input property"));

        let mut sparse = tool();
        sparse.cli.arguments.get_mut("widget_id").unwrap().position = Some(1);
        assert!(PreparedCliTree::prepare(&catalog(sparse))
            .unwrap_err()
            .to_string()
            .contains("dense from 0"));

        let mut duplicate = tool();
        duplicate.cli.arguments.get_mut("tags").unwrap().aliases = vec!["tag".to_string()];
        assert!(PreparedCliTree::prepare(&catalog(duplicate))
            .unwrap_err()
            .to_string()
            .contains("shared by properties"));

        let mut reserved = tool();
        reserved.cli.arguments.get_mut("tags").unwrap().long = Some("harn-input".to_string());
        assert!(PreparedCliTree::prepare(&catalog(reserved))
            .unwrap_err()
            .to_string()
            .contains("reserved --harn-input"));

        let mut reserved_framework_flag = tool();
        reserved_framework_flag
            .cli
            .arguments
            .get_mut("tags")
            .unwrap()
            .short = Some('h');
        assert!(PreparedCliTree::prepare(&catalog(reserved_framework_flag))
            .unwrap_err()
            .to_string()
            .contains("reserved -h"));

        let mut reserved_command = catalog(tool());
        reserved_command.cli.as_mut().unwrap().commands[0].aliases = vec!["help".to_string()];
        assert!(PreparedCliTree::prepare(&reserved_command)
            .unwrap_err()
            .to_string()
            .contains("reserved by the generated command framework"));

        let mut invalid_alias = catalog(tool());
        invalid_alias.cli.as_mut().unwrap().commands[0].aliases = vec!["bad alias".to_string()];
        assert!(PreparedCliTree::prepare(&invalid_alias)
            .unwrap_err()
            .to_string()
            .contains("not a portable command component"));

        let mut empty_help_group = tool();
        empty_help_group
            .cli
            .arguments
            .get_mut("tags")
            .unwrap()
            .help_group = Some("  ".to_string());
        assert!(PreparedCliTree::prepare(&catalog(empty_help_group))
            .unwrap_err()
            .to_string()
            .contains("help_group must not be empty"));

        let mut invalid_mcp_only = tool();
        invalid_mcp_only.governance.audiences = vec![ToolAudience::Mcp];
        invalid_mcp_only
            .cli
            .arguments
            .insert("missing".to_string(), ToolCliArgumentSpec::default());
        assert!(PreparedCliTree::prepare(&catalog(invalid_mcp_only))
            .unwrap_err()
            .to_string()
            .contains("unknown input property"));

        let mut weakened = tool();
        weakened
            .cli
            .arguments
            .get_mut("widget_id")
            .unwrap()
            .repeatable = true;
        assert!(PreparedCliTree::prepare(&catalog(weakened))
            .unwrap_err()
            .to_string()
            .contains("only when its schema type is array"));
    }
}
