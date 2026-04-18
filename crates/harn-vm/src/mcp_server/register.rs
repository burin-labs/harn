use std::cell::RefCell;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::defs::{McpPromptArgDef, McpPromptDef, McpResourceDef, McpResourceTemplateDef};

thread_local! {
    /// Stores the tool registry set by `mcp_tools` / `mcp_serve`.
    static MCP_SERVE_REGISTRY: RefCell<Option<VmValue>> = const { RefCell::new(None) };
    /// Static resources registered by `mcp_resource`.
    static MCP_SERVE_RESOURCES: RefCell<Vec<McpResourceDef>> = const { RefCell::new(Vec::new()) };
    /// Resource templates registered by `mcp_resource_template`.
    static MCP_SERVE_RESOURCE_TEMPLATES: RefCell<Vec<McpResourceTemplateDef>> = const { RefCell::new(Vec::new()) };
    /// Prompts registered by `mcp_prompt`.
    static MCP_SERVE_PROMPTS: RefCell<Vec<McpPromptDef>> = const { RefCell::new(Vec::new()) };
}

/// Register all MCP server builtins on a VM.
pub fn register_mcp_server_builtins(vm: &mut Vm) {
    fn register_tools_impl(args: &[VmValue]) -> Result<VmValue, VmError> {
        let registry = args.first().cloned().ok_or_else(|| {
            VmError::Runtime("mcp_tools: requires a tool_registry argument".into())
        })?;
        if let VmValue::Dict(d) = &registry {
            match d.get("_type") {
                Some(VmValue::String(t)) if &**t == "tool_registry" => {}
                _ => {
                    return Err(VmError::Runtime(
                        "mcp_tools: argument must be a tool registry (created with tool_registry())"
                            .into(),
                    ));
                }
            }
        } else {
            return Err(VmError::Runtime(
                "mcp_tools: argument must be a tool registry".into(),
            ));
        }
        MCP_SERVE_REGISTRY.with(|cell| {
            *cell.borrow_mut() = Some(registry);
        });
        Ok(VmValue::Nil)
    }

    vm.register_builtin("mcp_tools", |args, _out| register_tools_impl(args));
    // `mcp_serve` is the old name; kept as an alias.
    vm.register_builtin("mcp_serve", |args, _out| register_tools_impl(args));

    // mcp_resource({uri, name, text, description?, mime_type?}) -> nil
    vm.register_builtin("mcp_resource", |args, _out| {
        let dict = match args.first() {
            Some(VmValue::Dict(d)) => d,
            _ => {
                return Err(VmError::Runtime(
                    "mcp_resource: argument must be a dict with {uri, name, text}".into(),
                ));
            }
        };

        let uri = dict
            .get("uri")
            .map(|v| v.display())
            .ok_or_else(|| VmError::Runtime("mcp_resource: 'uri' is required".into()))?;
        let name = dict
            .get("name")
            .map(|v| v.display())
            .ok_or_else(|| VmError::Runtime("mcp_resource: 'name' is required".into()))?;
        let title = dict.get("title").map(|v| v.display());
        let description = dict.get("description").map(|v| v.display());
        let mime_type = dict.get("mime_type").map(|v| v.display());
        let text = dict
            .get("text")
            .map(|v| v.display())
            .ok_or_else(|| VmError::Runtime("mcp_resource: 'text' is required".into()))?;

        MCP_SERVE_RESOURCES.with(|cell| {
            cell.borrow_mut().push(McpResourceDef {
                uri,
                name,
                title,
                description,
                mime_type,
                text,
            });
        });

        Ok(VmValue::Nil)
    });

    // mcp_resource_template({uri_template, name, handler, description?, mime_type?}) -> nil
    // The handler receives a dict of URI template arguments and returns a string.
    vm.register_builtin("mcp_resource_template", |args, _out| {
        let dict = match args.first() {
            Some(VmValue::Dict(d)) => d,
            _ => {
                return Err(VmError::Runtime(
                    "mcp_resource_template: argument must be a dict".into(),
                ));
            }
        };

        let uri_template = dict
            .get("uri_template")
            .map(|v| v.display())
            .ok_or_else(|| {
                VmError::Runtime("mcp_resource_template: 'uri_template' is required".into())
            })?;
        let name = dict
            .get("name")
            .map(|v| v.display())
            .ok_or_else(|| VmError::Runtime("mcp_resource_template: 'name' is required".into()))?;
        let title = dict.get("title").map(|v| v.display());
        let description = dict.get("description").map(|v| v.display());
        let mime_type = dict.get("mime_type").map(|v| v.display());
        let handler = match dict.get("handler") {
            Some(VmValue::Closure(c)) => (**c).clone(),
            _ => {
                return Err(VmError::Runtime(
                    "mcp_resource_template: 'handler' closure is required".into(),
                ));
            }
        };

        MCP_SERVE_RESOURCE_TEMPLATES.with(|cell| {
            cell.borrow_mut().push(McpResourceTemplateDef {
                uri_template,
                name,
                title,
                description,
                mime_type,
                handler,
            });
        });

        Ok(VmValue::Nil)
    });

    // mcp_prompt({name, handler, description?, arguments?}) -> nil
    vm.register_builtin("mcp_prompt", |args, _out| {
        let dict = match args.first() {
            Some(VmValue::Dict(d)) => d,
            _ => {
                return Err(VmError::Runtime(
                    "mcp_prompt: argument must be a dict with {name, handler}".into(),
                ));
            }
        };

        let name = dict
            .get("name")
            .map(|v| v.display())
            .ok_or_else(|| VmError::Runtime("mcp_prompt: 'name' is required".into()))?;
        let title = dict.get("title").map(|v| v.display());
        let description = dict.get("description").map(|v| v.display());

        let handler = match dict.get("handler") {
            Some(VmValue::Closure(c)) => (**c).clone(),
            _ => {
                return Err(VmError::Runtime(
                    "mcp_prompt: 'handler' closure is required".into(),
                ));
            }
        };

        let arguments = dict.get("arguments").and_then(|v| {
            if let VmValue::List(list) = v {
                let args: Vec<McpPromptArgDef> = list
                    .iter()
                    .filter_map(|item| {
                        if let VmValue::Dict(d) = item {
                            Some(McpPromptArgDef {
                                name: d.get("name").map(|v| v.display()).unwrap_or_default(),
                                description: d.get("description").map(|v| v.display()),
                                required: matches!(d.get("required"), Some(VmValue::Bool(true))),
                            })
                        } else {
                            None
                        }
                    })
                    .collect();
                if args.is_empty() {
                    None
                } else {
                    Some(args)
                }
            } else {
                None
            }
        });

        MCP_SERVE_PROMPTS.with(|cell| {
            cell.borrow_mut().push(McpPromptDef {
                name,
                title,
                description,
                arguments,
                handler,
            });
        });

        Ok(VmValue::Nil)
    });
}

// Thread-local accessors used by the CLI after pipeline execution.

pub fn take_mcp_serve_registry() -> Option<VmValue> {
    MCP_SERVE_REGISTRY.with(|cell| cell.borrow_mut().take())
}

pub fn take_mcp_serve_resources() -> Vec<McpResourceDef> {
    MCP_SERVE_RESOURCES.with(|cell| cell.borrow_mut().drain(..).collect())
}

pub fn take_mcp_serve_resource_templates() -> Vec<McpResourceTemplateDef> {
    MCP_SERVE_RESOURCE_TEMPLATES.with(|cell| cell.borrow_mut().drain(..).collect())
}

pub fn take_mcp_serve_prompts() -> Vec<McpPromptDef> {
    MCP_SERVE_PROMPTS.with(|cell| cell.borrow_mut().drain(..).collect())
}
