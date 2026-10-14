use super::compiler_fixture_bundle;
use super::*;

fn has_diagnostic(output: &std::process::Output, code: &str) -> bool {
    String::from_utf8_lossy(&output.stderr).lines().any(|line| {
        serde_json::from_str::<serde_json::Value>(line)
            .ok()
            .and_then(|diagnostic| diagnostic["code"]["code"].as_str().map(str::to_owned))
            .as_deref()
            == Some(code)
    })
}

fn compile_fixture(source: &str, label: &str) -> std::process::Output {
    let fixture = tempfile::tempdir().expect("isolated compiler fixture");
    let source_path = fixture.path().join("consumer.rs");
    std::fs::write(&source_path, source).expect("write generated consumer");
    let dependencies = compiler_fixture_bundle::dependency_directory();
    Command::new("rustc")
        .args([
            "--edition=2021",
            "--crate-name",
            label,
            "--emit=metadata",
            "--error-format=json",
        ])
        .arg("--extern")
        .arg(format!(
            "serde={}",
            compiler_fixture_bundle::dependency("serde").display()
        ))
        .arg("--extern")
        .arg(format!(
            "harn_vm={}",
            compiler_fixture_bundle::dependency("harn_vm").display()
        ))
        .arg("-L")
        .arg(format!("dependency={}", dependencies.display()))
        .arg("--out-dir")
        .arg(fixture.path())
        .arg(source_path)
        .output()
        .expect("run real Rust compiler")
}

#[test]
fn external_consumers_must_handle_future_public_taxonomy_variants() {
    let cases = [
        (
            "harn_vm::llm::api::LlmErrorKind",
            harn_vm::llm::api::LlmErrorKind::ALL
                .iter()
                .map(|v| format!("{v:?}"))
                .collect::<Vec<_>>(),
        ),
        (
            "harn_vm::llm::api::LlmErrorReason",
            harn_vm::llm::api::LlmErrorReason::ALL
                .iter()
                .map(|v| format!("{v:?}"))
                .collect(),
        ),
        (
            "harn_vm::value::ErrorCategory",
            harn_vm::value::ErrorCategory::ALL
                .iter()
                .map(|v| format!("{v:?}"))
                .collect(),
        ),
        (
            "harn_vm::llm::AgentTerminalClass",
            harn_vm::llm::AgentTerminalClass::ALL
                .iter()
                .map(|v| format!("{v:?}"))
                .collect(),
        ),
        (
            "harn_vm::agent_events::AgentTerminalKind",
            harn_vm::agent_events::AgentTerminalKind::ALL
                .iter()
                .map(|v| format!("{v:?}"))
                .collect(),
        ),
        (
            "harn_vm::agent_events::ToolCallErrorCategory",
            harn_vm::agent_events::ToolCallErrorCategory::ALL
                .iter()
                .map(|v| format!("{v:?}"))
                .collect(),
        ),
    ];
    for (index, (owner, variants)) in cases.into_iter().enumerate() {
        assert!(
            !variants.is_empty(),
            "the exact owner registry must be measured"
        );
        let arms = variants
            .iter()
            .map(|v| format!("{owner}::{v} => (),"))
            .collect::<String>();
        let exhaustive =
            format!("fn probe(value: {owner}) {{ match value {{ {arms} }} }} fn main() {{}}");
        let output = compile_fixture(&exhaustive, &format!("owner_exhaustive_{index}"));
        assert!(
            !output.status.success(),
            "{owner} permits an exhaustive downstream match"
        );
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        assert!(has_diagnostic(&output, "E0004"), "{owner}: {diagnostics}");
        let fallback = format!(
            "fn probe(value: {owner}) {{ match value {{ {arms} _ => (), }} }} fn main() {{}}"
        );
        let output = compile_fixture(&fallback, &format!("owner_fallback_{index}"));
        assert!(
            output.status.success(),
            "{owner}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    compiler_fixture_bundle::stage_if_requested();
}

#[test]
fn external_crate_keeps_compiling_after_an_actual_reason_declaration_gains_a_variant() {
    let source = std::fs::read_to_string(
        std::path::Path::new(
            &std::env::var("CARGO_MANIFEST_DIR").expect("runtime workspace mapping"),
        )
        .join("../harn-vm/src/llm/api/errors.rs"),
    )
    .expect("read the actual public reason declaration");
    let start = source
        .find("pub enum LlmErrorReason {")
        .expect("public reason enum");
    let annotations = source
        .get(..start)
        .expect("reason prefix")
        .rfind("#[derive(")
        .expect("reason attributes");
    let end = start
        + source
            .get(start..)
            .expect("reason suffix")
            .find("\n}")
            .expect("reason declaration end")
        + 2;
    let declaration = source.get(annotations..end).expect("reason declaration");
    // Isolate the real, dependency-free declaration, rather than a hand-written
    // lookalike enum. The new variant is the simulated next owner release.
    assert!(declaration.contains("#[non_exhaustive]"));
    let future = format!(
        "{}\n    FutureFixture,\n}}",
        declaration
            .get(..declaration.len() - 1)
            .expect("reason body")
    );
    let consumer = "extern crate fixture_owner; use fixture_owner::LlmErrorReason; fn classify(value: LlmErrorReason) -> bool { match value { LlmErrorReason::RateLimit => true, _ => false } } fn main() { assert!(!classify(fixture_owner::unfamiliar())); }";
    for (declaration, variant) in [(declaration, "Unknown"), (future.as_str(), "FutureFixture")] {
        let fixture = tempfile::tempdir().expect("external crate outside the workspace");
        let library = fixture.path().join("owner.rs");
        let archive = fixture.path().join("libfixture_owner.rlib");
        std::fs::write(&library, format!("{declaration}\npub fn unfamiliar() -> LlmErrorReason {{ LlmErrorReason::{variant} }}")).unwrap();
        let output = Command::new("rustc")
            .args([
                "--edition=2021",
                "--crate-type=rlib",
                "--crate-name=fixture_owner",
            ])
            .arg(&library)
            .arg("-o")
            .arg(&archive)
            .output()
            .expect("compile owner crate");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let downstream = fixture.path().join("consumer.rs");
        let binary = fixture
            .path()
            .join(format!("consumer{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&downstream, consumer).unwrap();
        let output = Command::new("rustc")
            .args([
                "--edition=2021",
                "--crate-name=fixture_consumer",
                "--extern",
            ])
            .arg(format!("fixture_owner={}", archive.display()))
            .arg(&downstream)
            .arg("-o")
            .arg(&binary)
            .output()
            .expect("compile unchanged external consumer");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = Command::new(binary)
            .output()
            .expect("run external fallback");
        assert!(
            output.status.success(),
            "future reason must reach the fallback"
        );
    }
}

#[test]
fn regenerated_same_crate_open_enum_requires_a_wildcard_for_new_known_values() {
    let values = ["rate_limit".to_owned(), "unknown".to_owned()];
    let old = rust_open_string_enum("FixtureReason", "Fixture vocabulary.", &values);
    let mut future_values = values.to_vec();
    future_values.push("future_reason".to_owned());
    let future = rust_open_string_enum("FixtureReason", "Fixture vocabulary.", &future_values);
    let exhaustive = "fn classify(value: FixtureReason) -> bool { match value { FixtureReason::RateLimit => true, FixtureReason::Unknown | FixtureReason::Unrecognized(_) => false } } fn main() {}";
    let wildcard = "fn classify(value: FixtureReason) -> bool { match value { FixtureReason::RateLimit => true, _ => false } } fn main() {}";
    let source = |binding: &str, consumer: &str| {
        format!("use serde::{{Serialize, Deserialize}};\n{binding}\n{consumer}\n")
    };
    let old_exhaustive = compile_fixture(&source(&old, exhaustive), "old_exhaustive");
    assert!(
        old_exhaustive.status.success(),
        "{}",
        String::from_utf8_lossy(&old_exhaustive.stderr)
    );
    let future_exhaustive = compile_fixture(&source(&future, exhaustive), "future_exhaustive");
    assert!(
        !future_exhaustive.status.success(),
        "new known variant must invalidate an exhaustive vendored match"
    );
    assert!(
        has_diagnostic(&future_exhaustive, "E0004"),
        "{}",
        String::from_utf8_lossy(&future_exhaustive.stderr)
    );
    for (label, binding) in [("old_wildcard", old), ("future_wildcard", future)] {
        let output = compile_fixture(&source(&binding, wildcard), label);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
