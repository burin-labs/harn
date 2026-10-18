pub use harn_vm_core::bench_internals::*;

/// Bench-only access to the `harn_entry` Rust↔`.harn` boundary crossing
/// (`crates/harn-vm/src/stdlib/harn_entry.rs`). The stage-loop inversion
/// re-architecture multiplies the number of these crossings (per tool
/// call, per turn, per stage attempt), so
/// `bench/orchestration/bench_harn_entry_crossing.rs` regression-gates the
/// seam. Both production entry points are covered:
///
/// - [`call_export_by_name`] — the `&[VmValue]`-direct path (no JSON
///   marshalling).
/// - [`call_export_typed`] — the serde path used by typed hosts:
///   `json_to_vm_value` on the way in, `vm_value_to_json` + serde
///   deserialize on the way out.
///
/// The target export is `std/semver::parse` — pure, no nested imports,
/// trivial body — so the measurement isolates the crossing itself
/// (child-VM clone, module lookup/instantiation, closure call, marshal)
/// rather than callee work.
pub mod harn_entry_crossing {
    use crate::value::{VmError, VmValue};
    use crate::vm::AsyncBuiltinCtx;
    use crate::Vm;

    /// Import path of the trivial stdlib module the bench crosses into.
    pub const IMPORT_PATH: &str = "std/semver";
    /// Stdlib module name as keyed in the VM module cache.
    pub const STDLIB_MODULE: &str = "semver";
    /// Export invoked on every crossing.
    pub const EXPORT_NAME: &str = "parse";

    /// Typed mirror of `std/semver::parse`'s `Version` return shape, so
    /// [`call_export_typed`] pays the same serde-deserialize tax a real
    /// typed host seam pays.
    #[derive(Debug, serde::Deserialize)]
    pub struct ParsedVersion {
        pub major: i64,
        pub minor: i64,
        pub patch: i64,
    }

    /// True when the *parent* VM's module cache already holds the
    /// instantiated stdlib module — i.e. a crossing from this VM takes
    /// the warm path. Child VMs inherit the cache by `Arc` COW
    /// (`state.rs` `child_vm`); when the parent cache misses, every
    /// crossing replays module instantiation into the child's copy and
    /// drops it on return (`modules.rs` `instantiate_module`).
    pub fn stdlib_module_is_cached(vm: &Vm, module: &str) -> bool {
        let synthetic = std::path::PathBuf::from(format!("<stdlib>/{module}.harn"));
        harn_vm_core::bench_internals::module_is_cached(vm, &synthetic)
    }

    /// Load `import_path` on the parent VM so its module cache carries
    /// the instantiated module into every child crossing (the warm
    /// configuration).
    pub async fn warm_parent_module_cache(vm: &mut Vm, import_path: &str) -> Result<(), VmError> {
        vm.load_module_exports_from_import(import_path)
            .await
            .map(|_| ())
    }

    /// One `VmValue`-direct boundary crossing
    /// (`call_harn_export_by_name`).
    pub async fn call_export_by_name(
        ctx: &AsyncBuiltinCtx,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        crate::stdlib::harn_entry::call_harn_export_by_name(
            ctx,
            IMPORT_PATH,
            EXPORT_NAME,
            "bench_harn_entry_crossing_by_name",
            args,
        )
        .await
    }

    /// The production text-tool parse seam every tool-bearing LLM response
    /// takes (`llm::api::parse_text_tools_with_harn`): a typed crossing into
    /// `std/llm/tool_parse`, whose import closure no other stdlib module
    /// imports, so a parent VM never holds it warm. Returns the parsed prose.
    pub async fn parse_text_tool_calls(
        ctx: &AsyncBuiltinCtx,
        text: &str,
    ) -> Result<String, VmError> {
        crate::llm::api::parse_text_tools_with_harn(Some(ctx), text, None, "")
            .await
            .map(|parsed| parsed.prose)
    }

    /// One typed boundary crossing (`call_harn_export_typed`), including
    /// the full JSON double-marshal + serde deserialize.
    pub async fn call_export_typed(
        ctx: &AsyncBuiltinCtx,
        payload: serde_json::Value,
    ) -> Result<ParsedVersion, VmError> {
        crate::stdlib::harn_entry::call_harn_export_typed(
            ctx,
            IMPORT_PATH,
            EXPORT_NAME,
            "bench_harn_entry_crossing_typed",
            payload,
        )
        .await
    }
}

/// Bench-only access to the transcript projection path
/// (`crates/harn-vm/src/stdlib/transcript_project.rs`) exercised once
/// per agent turn. `bench/orchestration/bench_transcript_projection.rs`
/// drives it with synthetic transcripts at ~10k/50k/100k tokens.
pub mod transcript_projection {
    use crate::value::{VmDictExt, VmError, VmValue};
    use crate::vm::AsyncBuiltinCtx;

    /// Build a transcript-shaped `VmValue` (`{_type: "transcript",
    /// messages: [...]}`) from provider-format JSON messages, matching
    /// what `is_transcript_value` / `transcript_message_list` expect.
    pub fn transcript_value_from_messages(messages: &[serde_json::Value]) -> VmValue {
        let mut dict = crate::value::DictMap::new();
        dict.put_str("_type", "transcript");
        dict.put(
            "messages",
            VmValue::list(
                messages
                    .iter()
                    .map(crate::schema::json_to_vm_value)
                    .collect(),
            ),
        );
        VmValue::dict(dict)
    }

    /// Run one projection exactly as the `transcript_project` builtin
    /// does per turn: parse the options into a policy, project, and
    /// materialize the result dict. Returns the projected result value.
    pub async fn project_for_bench(
        ctx: &AsyncBuiltinCtx,
        transcript: &VmValue,
        options: &serde_json::Value,
    ) -> Result<VmValue, VmError> {
        let dict = transcript.as_dict().ok_or_else(|| {
            VmError::Runtime("bench transcript fixture must be a dict".to_string())
        })?;
        let options_vm = crate::schema::json_to_vm_value(options);
        let policy = crate::stdlib::transcript_project::parse_projection_options(&options_vm)?;
        let result =
            crate::stdlib::transcript_project::project_transcript(Some(ctx), dict, &policy).await?;
        Ok(crate::stdlib::transcript_project::result_to_vm(
            &result, &policy,
        ))
    }
}
