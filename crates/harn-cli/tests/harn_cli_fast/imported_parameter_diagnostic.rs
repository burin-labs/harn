use std::fs;

use crate::test_util::process::run_harn_e2e as run;

#[test]
fn imported_parameter_failure_names_the_function_and_fails_loudly() {
    let temp = tempfile::tempdir().expect("tempdir");
    let helper = temp.path().join("helper.harn");
    let entry = temp.path().join("entry.harn");
    fs::write(
        &helper,
        "pub fn named_helper(file_id: string) -> string { return file_id }\n",
    )
    .expect("write imported helper");
    fs::write(
        &entry,
        r#"import { named_helper } from "./helper"

fn main(harness: Harness) {
  const receipt = json_parse("{\"file_id\": null}")
  harness.stdio.println(named_helper(receipt["file_id"]))
}
"#,
    )
    .expect("write entry script");

    let outcome = run(&["run", entry.to_str().expect("UTF-8 entry path")], &[]);

    assert_ne!(
        outcome.exit_code, 0,
        "runtime type failure must exit nonzero"
    );
    assert!(
        outcome.stdout.is_empty(),
        "runtime failure must not emit a false-success payload: {}",
        outcome.stdout
    );
    assert!(
        outcome.stderr.contains(
            "TypeError: function 'named_helper' parameter 'file_id' expected string, got nil"
        ),
        "stderr did not identify the imported function: {}",
        outcome.stderr
    );
}
