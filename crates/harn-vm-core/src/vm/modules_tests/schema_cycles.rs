use super::*;

#[tokio::test]
async fn schema_import_cycles_settle_without_weakening_validation() {
    for (import, schema_expr) in [
        ("import { Wrapped } from \"./b\"", "Wrapped"),
        ("import \"./b\"", "Wrapped"),
        ("import * as b from \"./b\"", "b.Wrapped"),
    ] {
        for first in ["a.harn", "b.harn"] {
            let root = tempfile::tempdir().unwrap();
            std::fs::write(
                root.path().join("a.harn"),
                format!(
                    "{import}\npub type Base = {{value: int}}\npub fn wrapped_schema() {{ return {schema_expr} }}\n"
            ),
            )
            .unwrap();
            std::fs::write(
                root.path().join("b.harn"),
                "import { Base } from \"./a\"\nimport * as a from \"./a\"\n\
                 pub type Wrapped = Base\npub fn base_schema() { return a.Base }\n",
            )
            .unwrap();
            let mut vm = super::super::super::test_runtime::new_vm();
            vm.loaded_module_for_path(&root.path().join(first))
                .await
                .unwrap();
            for (module, function) in [("a.harn", "wrapped_schema"), ("b.harn", "base_schema")] {
                let schema = vm
                    .call_module_export(
                        root.path().join(module).to_str().unwrap(),
                        function,
                        "cycle",
                        &[],
                    )
                    .await
                    .unwrap();
                for (value, expected) in [
                    (VmValue::Int(1), true),
                    (VmValue::String("bad".into()), false),
                ] {
                    let record = VmValue::dict(BTreeMap::from([("value".to_string(), value)]));
                    assert_eq!(
                        crate::schema::schema_is_value(&record, &schema).unwrap(),
                        expected,
                        "{import}, first {first}, via {module}"
                    );
                }
            }
        }
    }
}
