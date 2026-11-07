use serde_json::Value;

pub(crate) fn read_events(directory: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(directory.join("llm_transcript.jsonl"))
        .expect("provider transcript")
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid transcript JSON"))
        .collect()
}

pub(crate) async fn census(events: &[Value]) -> Value {
    let source = r#"
import { provider_call_census, provider_transcript_normalize } from "std/agent/transcript"
fn main(harness: Harness) {
  const records = provider_transcript_normalize(json_parse(argv[0]))
  harness.stdio.println(json_stringify(provider_call_census(records, true)))
}
"#;
    let path = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/llm/agent_observe/transcript_census.harn"
    ));
    let chunk = crate::compile_source_at_path(source, path, None)
        .expect("public transcript facade compiles");
    let mut vm = crate::test_support::vm_with_stdlib();
    vm.set_global(
        "argv",
        crate::value::VmValue::list(vec![crate::value::VmValue::string(
            serde_json::to_string(events).unwrap(),
        )]),
    );
    tokio::task::LocalSet::new()
        .run_until(vm.execute(&chunk))
        .await
        .expect("public transcript census executes");
    vm.output()
        .lines()
        .find_map(|line| serde_json::from_str(line.strip_prefix("[harn] ").unwrap_or(line)).ok())
        .expect("one census report")
}
