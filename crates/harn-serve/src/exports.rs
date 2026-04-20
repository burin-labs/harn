use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use harn_parser::{Node, TypeExpr};

use crate::DispatchError;

#[derive(Clone, Debug, PartialEq)]
pub struct ExportedParam {
    pub name: String,
    pub type_expr: Option<TypeExpr>,
    pub input_schema: serde_json::Value,
    pub has_default: bool,
    pub rest: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExportedFunction {
    pub name: String,
    pub params: Vec<ExportedParam>,
    pub return_type: Option<TypeExpr>,
    pub input_schema: serde_json::Value,
    pub output_schema: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExportCatalog {
    pub script_path: PathBuf,
    pub functions: BTreeMap<String, ExportedFunction>,
}

impl ExportCatalog {
    pub fn from_path(path: &Path) -> Result<Self, DispatchError> {
        let source = fs::read_to_string(path).map_err(|error| {
            DispatchError::Io(format!("failed to read {}: {error}", path.display()))
        })?;
        let program = harn_parser::parse_source(&source).map_err(|error| {
            DispatchError::Validation(format!("failed to parse {}: {error}", path.display()))
        })?;

        let mut functions = BTreeMap::new();
        for node in &program {
            let (_, inner) = harn_parser::peel_attributes(node);
            let Node::FnDecl {
                name,
                params,
                return_type,
                is_pub,
                ..
            } = &inner.node
            else {
                continue;
            };
            if !*is_pub {
                continue;
            }

            let exported_params = params
                .iter()
                .map(|param| ExportedParam {
                    name: param.name.clone(),
                    type_expr: param.type_expr.clone(),
                    input_schema: param
                        .type_expr
                        .as_ref()
                        .and_then(harn_vm::json_schema_for_type_expr)
                        .unwrap_or_else(|| serde_json::json!({})),
                    has_default: param.default_value.is_some(),
                    rest: param.rest,
                })
                .collect::<Vec<_>>();

            functions.insert(
                name.clone(),
                ExportedFunction {
                    name: name.clone(),
                    params: exported_params,
                    return_type: return_type.clone(),
                    input_schema: harn_vm::json_schema_for_typed_params(params),
                    output_schema: return_type
                        .as_ref()
                        .and_then(harn_vm::json_schema_for_type_expr),
                },
            );
        }

        Ok(Self {
            script_path: path.to_path_buf(),
            functions,
        })
    }

    pub fn function(&self, name: &str) -> Option<&ExportedFunction> {
        self.functions.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_catalog_only_includes_public_functions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.harn");
        std::fs::write(
            &path,
            r#"
fn hidden() { return "nope" }
pub fn greet(name: string, excited: bool = false) -> string {
  if excited { return "hi!" }
  return name
}
"#,
        )
        .expect("write script");

        let catalog = ExportCatalog::from_path(&path).expect("catalog");
        assert!(catalog.function("hidden").is_none());
        let greet = catalog.function("greet").expect("greet export");
        assert_eq!(greet.params.len(), 2);
        assert_eq!(greet.input_schema["type"], "object");
        assert_eq!(
            greet.output_schema.as_ref().expect("output")["type"],
            "string"
        );
    }
}
