use super::*;
use std::sync::Arc;

struct TrustedHostDispatchBridge;

impl crate::HostCallBridge for TrustedHostDispatchBridge {
    fn dispatch<'a>(
        &'a self,
        capability: &'a str,
        operation: &'a str,
        _params: &'a crate::value::DictMap,
    ) -> crate::stdlib::host::HostCallDispatchFuture<'a> {
        assert_eq!(capability, "cloud");
        assert_eq!(operation, "echo");
        crate::host_call_ready(Ok(Some(VmValue::string("bridged"))))
    }
}

#[test]
fn trusted_host_dispatch_is_explicit_transitive_and_executable() {
    let runtime = crate::test_support::current_thread_runtime();
    let temp = tempfile::tempdir().expect("tempdir");
    let helper = temp.path().join("helper.harn");
    let entry = temp.path().join("entry.harn");
    std::fs::write(
        &helper,
        "pub fn dispatch(payload) { return host_call(\"cloud.echo\", payload) }\n",
    )
    .expect("write helper");
    std::fs::write(
        &entry,
        "import { dispatch } from \"./helper\"\npub fn route(payload) { return dispatch(payload) }\n",
    )
    .expect("write entry");

    runtime.block_on(async {
        let mut ordinary = crate::new_vm();
        crate::stdlib::register_vm_stdlib(&mut ordinary);
        let error = ordinary
            .load_module_exports(&entry)
            .await
            .expect_err("ordinary module graph must reject host_call while compiling");
        assert!(error.to_string().contains("host_call"), "{error}");

        harn_clock::test_support::with_local_tasks(async {
            crate::set_host_call_bridge(Arc::new(TrustedHostDispatchBridge));
            let mut trusted = crate::new_vm();
            crate::stdlib::register_vm_stdlib(&mut trusted);
            trusted
                .enable_trusted_host_dispatch()
                .expect("fresh VM accepts explicit authority");
            let exports = trusted
                .load_module_exports(&entry)
                .await
                .expect("trusted graph loads transitively");
            let route = exports.get("route").expect("route exported");
            let value = trusted
                .call_closure_pub(route, &[VmValue::dict(crate::value::DictMap::default())])
                .await
                .expect("host-selected callable executes");
            crate::clear_host_call_bridge();
            assert_eq!(value.display(), "bridged");
        })
        .await;
    });
}

#[test]
fn unregistered_host_fixtures_require_explicit_test_local_authority() {
    let runtime = crate::test_support::current_thread_runtime();

    runtime.block_on(async {
        let mut vm = crate::new_vm();
        let VmValue::Harness(root) = crate::Harness::real().into_vm_value() else {
            panic!("real harness lowers to a VM handle");
        };
        let testing = root.sub_handle("testing").expect("testing sub-handle");
        let mut args = vec![
            VmValue::string("cloud"),
            VmValue::string("echo"),
            VmValue::string("fixture"),
            VmValue::Nil,
            VmValue::Bool(false),
        ];

        let error = vm
            .call_harness_method(&testing, "respond", &args)
            .await
            .expect_err("unknown host operation fails closed by default");
        assert!(
            error.to_string().contains("unregistered_ok=true"),
            "{error}"
        );

        args.push(VmValue::Bool(true));
        vm.call_harness_method(&testing, "respond", &args)
            .await
            .expect("explicit test-local host fixture is accepted");
        let result = crate::harness::native_harness(&root)
            .unwrap()
            .fixtures()
            .dispatch_host("cloud", "echo", &crate::value::DictMap::default())
            .expect("fixture matches")
            .expect("fixture succeeds");
        assert_eq!(result.display(), "fixture");
    });
}

#[test]
fn module_function_can_use_local_type_alias_as_schema_value() {
    let runtime = crate::test_support::current_thread_runtime();

    let result = runtime.block_on(async {
        let mut vm = crate::new_vm();
        crate::stdlib::register_vm_stdlib(&mut vm);
        let loaded = vm
            .load_module_exports_from_source(
                "<test>/schema_alias_module.harn",
                r#"
fn accepts_schema(schema) {
  return schema_report({name: "Ada"}, schema).ok
}

type UserShape = {name: string}

pub fn works() {
  return accepts_schema(UserShape)
}
"#,
            )
            .await
            .expect("module loads");
        let closure = Arc::clone(loaded.get("works").expect("works export exists"));
        vm.call_closure_pub(&closure, &[])
            .await
            .expect("module closure executes")
    });

    assert!(matches!(result, VmValue::Bool(true)), "{result:?}");
}

#[test]
fn imported_public_struct_exports_its_runtime_constructor() {
    let runtime = crate::test_support::current_thread_runtime();
    let temp = tempfile::tempdir().expect("tempdir");
    let types = temp.path().join("types.harn");
    let consumer = temp.path().join("consumer.harn");
    std::fs::write(&types, "pub struct Decision { allowed: bool }\n").expect("write type module");
    std::fs::write(
        &consumer,
        r#"
import { Decision } from "./types"

pub fn decide() -> Decision {
  return Decision({allowed: true})
}
"#,
    )
    .expect("write consumer module");

    runtime.block_on(async {
        let mut vm = crate::new_vm();
        crate::register_vm_stdlib(&mut vm);
        let exports = vm
            .load_module_exports(&consumer)
            .await
            .expect("consumer module loads");
        let decide = exports.get("decide").expect("decide export");
        let result = vm
            .call_closure_pub(decide, &[])
            .await
            .expect("imported constructor executes");

        assert_eq!(result.struct_name(), Some("Decision"));
        let fields = result.struct_fields_map().expect("struct fields");
        let Some(VmValue::Bool(allowed)) = fields.get("allowed") else {
            panic!(
                "expected bool field `allowed`, got {:?}",
                fields.get("allowed")
            );
        };
        assert!(*allowed, "expected `allowed` to be true");
    });
}

#[test]
fn wildcard_imported_source_callable_shadows_legacy_parser_builtin() {
    let runtime = crate::test_support::current_thread_runtime();
    let temp = tempfile::tempdir().expect("tempdir");
    let library = temp.path().join("library.harn");
    let consumer = temp.path().join("consumer.harn");
    std::fs::write(
        &library,
        r"pub fn context(value: string) -> string { return value }",
    )
    .expect("write library module");
    std::fs::write(
        &consumer,
        r#"
import "./library"

pub fn exercise() -> string {
  return context("wildcard")
}
"#,
    )
    .expect("write consumer module");

    runtime.block_on(async {
        let mut vm = crate::new_vm();
        crate::register_vm_stdlib(&mut vm);
        let exports = vm
            .load_module_exports(&consumer)
            .await
            .expect("wildcard source callable resolves before builtin policy");
        let exercise = exports.get("exercise").expect("exercise export");
        let result = vm
            .call_closure_pub(exercise, &[])
            .await
            .expect("wildcard source callable executes");
        assert!(matches!(
            result,
            VmValue::String(value) if value.as_str() == "wildcard"
        ));
    });
}

#[test]
fn imported_public_enum_exports_namespace_and_preserves_source_context() {
    let runtime = crate::test_support::current_thread_runtime();
    let temp = tempfile::tempdir().expect("tempdir");
    let library = temp.path().join("library.harn");
    let facade = temp.path().join("facade.harn");
    let consumer = temp.path().join("consumer.harn");
    let wildcard_consumer = temp.path().join("wildcard_consumer.harn");
    std::fs::write(
        &library,
        r"
pub enum Color {
  Ready(message: string)
  Empty
}

pub fn from_library(message: string) -> Color {
  return Color.Ready(message)
}
",
    )
    .expect("write enum module");
    std::fs::write(
        &facade,
        r#"
pub import { Color, from_library } from "./library"
"#,
    )
    .expect("write enum facade");
    std::fs::write(
        &consumer,
        r#"
import { Color, from_library } from "./facade"

pub fn exercise() -> string {
  const direct = Color.Ready("direct")
  const indirect = from_library("indirect")
  match direct {
    Color.Ready(message) -> {
      match indirect {
        Color.Ready(other) -> { return message + ":" + other }
        _ -> { return "indirect-mismatch" }
      }
    }
    _ -> { return "direct-mismatch" }
  }
}
"#,
    )
    .expect("write consumer module");
    std::fs::write(
        &wildcard_consumer,
        r#"
import "./facade"

pub fn exercise() -> string {
  const direct = Color.Ready("wildcard")
  match direct {
    Color.Ready(message) -> { return message }
    _ -> { return "wildcard-mismatch" }
  }
}
"#,
    )
    .expect("write wildcard consumer module");

    runtime.block_on(async {
        let mut vm = crate::new_vm();
        crate::register_vm_stdlib(&mut vm);
        let exports = vm
            .load_module_exports(&consumer)
            .await
            .expect("consumer module loads");
        let exercise = exports.get("exercise").expect("exercise export");
        let result = vm
            .call_closure_pub(exercise, &[])
            .await
            .expect("imported enum namespace and function execute");

        assert!(matches!(
            result,
            VmValue::String(value) if value.as_str() == "direct:indirect"
        ));

        let wildcard_exports = vm
            .load_module_exports(&wildcard_consumer)
            .await
            .expect("wildcard consumer module loads");
        let wildcard_exercise = wildcard_exports
            .get("exercise")
            .expect("wildcard exercise export");
        let wildcard_result = vm
            .call_closure_pub(wildcard_exercise, &[])
            .await
            .expect("wildcard imported enum namespace executes");
        assert!(matches!(
            wildcard_result,
            VmValue::String(value) if value.as_str() == "wildcard"
        ));
    });
}
