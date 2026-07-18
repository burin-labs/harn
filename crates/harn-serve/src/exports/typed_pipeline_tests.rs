use harn_parser::{ShapeField, TypeExpr};

use super::{ExportCatalog, ExportedCallableKind};

#[test]
fn export_catalog_preserves_typed_pipeline_schemas() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("server.harn");
    std::fs::write(
        &path,
        r"
pub pipeline deploy(config: {region: string, replicas: int}) -> bool {
  return config.replicas > 0
}
",
    )
    .expect("write script");
    let catalog = ExportCatalog::from_path(&path).expect("catalog");

    let deploy = catalog.function("deploy").expect("deploy pipeline");
    assert_eq!(deploy.kind, ExportedCallableKind::Pipeline);
    assert_eq!(deploy.params[0].name, "config");
    assert_eq!(
        deploy.params[0].type_expr,
        Some(TypeExpr::Shape(vec![
            ShapeField {
                name: "region".to_string(),
                type_expr: TypeExpr::Named("string".to_string()),
                optional: false,
            },
            ShapeField {
                name: "replicas".to_string(),
                type_expr: TypeExpr::Named("int".to_string()),
                optional: false,
            },
        ]))
    );
    assert_eq!(deploy.input_schema["type"], "object");
    assert_eq!(
        deploy.input_schema["properties"]["config"]["properties"]["replicas"]["type"],
        "integer"
    );
    assert_eq!(deploy.output_schema.as_ref().unwrap()["type"], "boolean");
}
