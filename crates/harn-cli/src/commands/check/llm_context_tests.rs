use super::*;

fn policy_context(dir: &Path, supports_effort: bool) -> LlmCheckContext {
    let modes = if supports_effort { "effort" } else { "enabled" };
    let levels = if supports_effort { "\"high\"" } else { "" };
    std::fs::write(
        dir.join("harn.toml"),
        format!(
            "[[capabilities.provider.test-provider]]\nmodel_match = \"configured-model\"\n\
             thinking_modes = [\"{modes}\"]\nreasoning_effort_supported = {supports_effort}\n\
             reasoning_effort_levels = [{levels}]\n"
        ),
    )
    .unwrap();
    LlmCheckContext::load(&dir.join("main.harn"))
}

fn effort_admission() -> Result<(), String> {
    harn_vm::llm::admit_reasoning_literals("test-provider", "configured-model", Some("high"), None)
}

#[test]
fn project_policy_scopes_restore_and_cache_keys_follow_edits() {
    let dir = tempfile::TempDir::new().unwrap();
    let accepted = policy_context(dir.path(), true);
    let refused = policy_context(dir.path(), false);
    assert_ne!(accepted.cache_key([0; 32]), refused.cache_key([0; 32]));
    accepted
        .with(|| {
            assert!(effort_admission().is_ok());
            let refusal = refused.with(effort_admission).unwrap().unwrap_err();
            assert!(refusal.contains("user capability overlay"), "{refusal}");
            assert!(effort_admission().is_ok(), "nested policy leaked");
        })
        .unwrap();
}

#[test]
fn malformed_project_policy_never_runs_the_check() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("harn.toml"), "[capabilities.provider").unwrap();
    let context = LlmCheckContext::load(&dir.path().join("main.harn"));
    assert!(context
        .with(|| panic!("invalid policy reached check"))
        .is_err());
}
