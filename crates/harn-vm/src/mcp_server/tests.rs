use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::VmValue;

use super::convert::{annotations_to_json, prompt_value_to_messages};
use super::tool_registry_to_mcp_tools;
use super::tools_schema::params_to_json_schema;
use super::uri::match_uri_template;

#[test]
fn test_params_to_json_schema_empty() {
    let schema = params_to_json_schema(None);
    assert_eq!(
        schema,
        serde_json::json!({ "type": "object", "properties": {} })
    );
}

#[test]
fn test_params_to_json_schema_with_params() {
    let mut params = BTreeMap::new();
    let mut param_def = BTreeMap::new();
    param_def.insert("type".to_string(), VmValue::String(Rc::from("string")));
    param_def.insert(
        "description".to_string(),
        VmValue::String(Rc::from("A file path")),
    );
    param_def.insert("required".to_string(), VmValue::Bool(true));
    params.insert("path".to_string(), VmValue::Dict(Rc::new(param_def)));

    let schema = params_to_json_schema(Some(&VmValue::Dict(Rc::new(params))));
    assert_eq!(
        schema,
        serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string", "description": "A file path" } },
            "required": ["path"]
        })
    );
}

#[test]
fn test_params_to_json_schema_simple_form() {
    let mut params = BTreeMap::new();
    params.insert("query".to_string(), VmValue::String(Rc::from("string")));
    let schema = params_to_json_schema(Some(&VmValue::Dict(Rc::new(params))));
    assert_eq!(
        schema["properties"]["query"]["type"],
        serde_json::json!("string")
    );
}

#[test]
fn test_tool_registry_to_mcp_tools_invalid() {
    assert!(tool_registry_to_mcp_tools(&VmValue::Nil).is_err());
}

#[test]
fn test_tool_registry_to_mcp_tools_empty() {
    let mut registry = BTreeMap::new();
    registry.insert("_type".into(), VmValue::String(Rc::from("tool_registry")));
    registry.insert("tools".into(), VmValue::List(Rc::new(Vec::new())));
    let result = tool_registry_to_mcp_tools(&VmValue::Dict(Rc::new(registry)));
    assert!(result.unwrap().is_empty());
}

#[test]
fn test_prompt_value_to_messages_string() {
    let msgs = prompt_value_to_messages(&VmValue::String(Rc::from("hello")));
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[0]["content"]["text"], "hello");
}

#[test]
fn test_prompt_value_to_messages_list() {
    let items = vec![
        VmValue::Dict(Rc::new({
            let mut d = BTreeMap::new();
            d.insert("role".into(), VmValue::String(Rc::from("user")));
            d.insert("content".into(), VmValue::String(Rc::from("hi")));
            d
        })),
        VmValue::Dict(Rc::new({
            let mut d = BTreeMap::new();
            d.insert("role".into(), VmValue::String(Rc::from("assistant")));
            d.insert("content".into(), VmValue::String(Rc::from("hello")));
            d
        })),
    ];
    let msgs = prompt_value_to_messages(&VmValue::List(Rc::new(items)));
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[1]["role"], "assistant");
}

#[test]
fn test_match_uri_template_simple() {
    let vars = match_uri_template("file:///{path}", "file:///foo/bar.rs").unwrap();
    assert_eq!(vars["path"], "foo/bar.rs");
}

#[test]
fn test_match_uri_template_multiple() {
    let vars = match_uri_template("db://{schema}/{table}", "db://public/users").unwrap();
    assert_eq!(vars["schema"], "public");
    assert_eq!(vars["table"], "users");
}

#[test]
fn test_match_uri_template_no_match() {
    assert!(match_uri_template("file:///{path}", "http://example.com").is_none());
}

#[test]
fn test_annotations_to_json() {
    let mut d = BTreeMap::new();
    d.insert("title".into(), VmValue::String(Rc::from("My Tool")));
    d.insert("readOnlyHint".into(), VmValue::Bool(true));
    d.insert("destructiveHint".into(), VmValue::Bool(false));
    let json = annotations_to_json(&VmValue::Dict(Rc::new(d))).unwrap();
    assert_eq!(json["title"], "My Tool");
    assert_eq!(json["readOnlyHint"], true);
    assert_eq!(json["destructiveHint"], false);
}

#[test]
fn test_annotations_empty_returns_none() {
    let d = BTreeMap::new();
    assert!(annotations_to_json(&VmValue::Dict(Rc::new(d))).is_none());
}
