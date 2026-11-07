use crate::value::VmDictExt;

use super::*;

#[test]
fn stdlib_tool_registry_from_carries_cli_metadata_through_import_abi() {
    let runtime = crate::test_support::current_thread_runtime();
    runtime.block_on(async {
        let mut vm = crate::new_vm();
        crate::register_vm_stdlib(&mut vm);
        let exports = vm
            .load_module_exports_from_import("std/tools")
            .await
            .expect("std/tools loads");
        let registry_from = exports
            .get("tool_registry_from")
            .expect("tool_registry_from is exported");
        assert_eq!(registry_from.func.params.len(), 2);

        let mut cli = crate::value::DictMap::new();
        cli.put("commands", VmValue::list(Vec::new()));
        let mut options = crate::value::DictMap::new();
        options.put("cli", VmValue::dict(cli));
        let registry = vm
            .call_closure_pub(
                registry_from,
                &[VmValue::list(Vec::new()), VmValue::dict(options)],
            )
            .await
            .expect("tool_registry_from executes");
        let catalog = crate::tool_registry::tool_registry_catalog(&registry)
            .expect("registry projects to the catalog");
        assert!(
            catalog.cli.is_some(),
            "CLI metadata survives the import ABI"
        );

        let wrapper = vm
            .load_module_exports_from_source(
                "<test>/tool-registry-wrapper.harn",
                r#"
import { tool_registry_from } from "std/tools"
pub fn project() {
  return tool_registry_from([], {cli: {commands: []}})
}
"#,
            )
            .await
            .expect("compiled import wrapper loads");
        let projected = vm
            .call_closure_pub(wrapper.get("project").expect("project is exported"), &[])
            .await
            .expect("compiled imported call executes");
        let projected_catalog = crate::tool_registry::tool_registry_catalog(&projected)
            .expect("compiled imported call returns a registry");
        assert!(
            projected_catalog.cli.is_some(),
            "compiled imported call retains the optional options record"
        );
    });
}
