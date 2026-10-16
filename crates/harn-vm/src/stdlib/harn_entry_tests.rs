//! Rust-to-Harn crossings reuse immutable stdlib module instances from the
//! execution tree's shared store (`vm/modules/shared_instances.rs`).

use crate::NativeBuiltinCtxExt;
use std::path::PathBuf;

use crate::value::VmValue;
use crate::vm::{AsyncBuiltinCtx, Vm};

fn stdlib_root() -> Vm {
    let mut vm = crate::new_vm();
    crate::stdlib::register_vm_stdlib(&mut vm);
    vm
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds")
}

fn stdlib_path(module: &str) -> PathBuf {
    PathBuf::from(format!("<stdlib>/{module}.harn"))
}

#[test]
fn harn_entry_crossings_instantiate_an_immutable_stdlib_closure_once() {
    // The text-tool parse seam crosses into `std/llm/tool_parse` once per
    // tool-bearing LLM response, and no stdlib module imports it, so the
    // parent VM never holds it warm. Every crossing used to instantiate its
    // whole closure into a transient child and drop it.
    let ctx = AsyncBuiltinCtx::for_test(stdlib_root());
    let runtime = runtime();
    const CROSSINGS: usize = 6;
    for _ in 0..CROSSINGS {
        let parsed = runtime
            .block_on(crate::llm::api::parse_text_tools_with_harn(
                Some(&ctx),
                "<assistant_prose>Reading the file now.</assistant_prose>",
                None,
                "",
            ))
            .expect("text-tool parse crossing succeeds");
        assert!(parsed.prose.contains("Reading the file now."));
        let version = runtime
            .block_on(super::call_harn_export_by_name(
                &ctx,
                "std/semver",
                "parse",
                "test",
                &[VmValue::string("v1.2.3")],
            ))
            .expect("semver crossing succeeds");
        assert!(version.as_dict().is_some());
    }

    let probe = ctx.child_vm();
    for module in ["llm/tool_parse", "llm/dialects", "semver"] {
        let path = stdlib_path(module);
        assert!(
            !probe.module_cached_for_test(&path),
            "the parent must stay cold for {module}, or this measures the warm path"
        );
        assert_eq!(
            probe.module_instantiation_count_for_test(&path),
            1,
            "{module} must instantiate once across {CROSSINGS} crossings"
        );
    }
}

#[test]
fn a_module_with_mutable_top_level_state_stays_fresh_per_cold_crossing() {
    // Negative control: `std/llm/handlers` keeps a mutable circuit memo, so
    // reusing its instance would carry state between crossings that today
    // each see a fresh module. It must keep instantiating per crossing while
    // its immutable imports are still shared.
    let ctx = AsyncBuiltinCtx::for_test(stdlib_root());
    let runtime = runtime();
    const CROSSINGS: usize = 3;
    for _ in 0..CROSSINGS {
        let key = runtime
            .block_on(super::call_harn_export_by_name(
                &ctx,
                "std/llm/handlers",
                "llm_cache_key",
                "test",
                &[VmValue::string("prompt")],
            ))
            .expect("handlers crossing succeeds");
        assert!(matches!(key, VmValue::String(_)));
    }

    let probe = ctx.child_vm();
    assert_eq!(
        probe.module_instantiation_count_for_test(&stdlib_path("llm/handlers")),
        CROSSINGS
    );
    assert_eq!(
        probe.module_instantiation_count_for_test(&stdlib_path("math")),
        1,
        "an immutable import of a mutable module is still shared"
    );

    // `std/agent/loop` reaches `std/llm/handlers`, so it is never shared.
    runtime.block_on(async {
        for _ in 0..2 {
            let mut child = ctx.child_vm();
            child
                .load_module_exports_from_import("std/agent/loop")
                .await
                .expect("agent loop loads");
        }
    });
    assert_eq!(
        probe.module_instantiation_count_for_test(&stdlib_path("agent/loop")),
        2
    );
}
