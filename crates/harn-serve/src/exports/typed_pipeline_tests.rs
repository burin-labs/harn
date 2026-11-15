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
            ShapeField::synthetic("region", TypeExpr::Named("string".to_string()), false),
            ShapeField::synthetic("replicas", TypeExpr::Named("int".to_string()), false),
        ]))
    );
    assert_eq!(deploy.input_schema["type"], "object");
    let schema = jsonschema::validator_for(&deploy.input_schema).unwrap();
    assert!(schema.is_valid(&serde_json::json!({"config":{"region":"west", "replicas":2}})));
    assert!(!schema.is_valid(&serde_json::json!({"config":{"region":"west", "replicas":"two"}})));
    assert!(!schema.is_valid(&serde_json::json!({"config":{"replicas":2}})));
    assert_eq!(deploy.output_schema.as_ref().unwrap()["type"], "boolean");
}
