use super::super::template_lint::{
    collect_lint_targets, lint_prompt_file_inner, lint_prompt_fix_file,
};
use super::unique_temp_dir;

#[test]
fn flags_provider_identity_branch() {
    let dir = unique_temp_dir("harn-lint-prompt-identity");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("sample.harn.prompt");
    std::fs::write(
        &file,
        "{{ if llm.provider == \"anthropic\" }}x{{ else }}y{{ end }}\n",
    )
    .unwrap();
    let outcome = lint_prompt_file_inner(&file, None, &[]);
    assert!(outcome.has_warning);
    assert!(!outcome.has_error);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn flags_unknown_filter() {
    let dir = unique_temp_dir("harn-lint-prompt-unknown-filter");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("sample.harn.prompt");
    std::fs::write(&file, "{{ name | uppr }}\n").unwrap();
    let outcome = lint_prompt_file_inner(&file, None, &[]);
    assert!(outcome.has_error);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fixes_unknown_filter() {
    let dir = unique_temp_dir("harn-lint-prompt-fix-unknown-filter");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("sample.harn.prompt");
    std::fs::write(&file, "{{ name | uppr }}\n").unwrap();

    let outcome = lint_prompt_fix_file(&file, None, &[]);

    assert!(!outcome.has_error);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "{{ name | upper }}\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn leaves_unfixable_unknown_filter_as_an_error() {
    let dir = unique_temp_dir("harn-lint-prompt-unfixable-unknown-filter");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("sample.harn.prompt");
    let source = "{{ name | completely_unknown }}\n";
    std::fs::write(&file, source).unwrap();

    let outcome = lint_prompt_fix_file(&file, None, &[]);

    assert!(outcome.has_error);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), source);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn flags_variant_explosion_above_threshold() {
    let dir = unique_temp_dir("harn-lint-prompt-explosion");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("sample.harn.prompt");
    let body: String = (0..4)
        .map(|i| {
            let flag = match i {
                0 => "native_tools",
                1 => "prefers_xml_scaffolding",
                2 => "supports_assistant_prefill",
                _ => "prefers_markdown_scaffolding",
            };
            format!("{{{{ if llm.capabilities.{flag} }}}}x{{{{ end }}}}\n")
        })
        .collect();
    std::fs::write(&file, body).unwrap();
    let outcome = lint_prompt_file_inner(&file, None, &[]);
    assert!(outcome.has_warning);
    let outcome_lifted = lint_prompt_file_inner(&file, Some(5), &[]);
    assert!(!outcome_lifted.has_warning);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn respects_disabled_rules() {
    let dir = unique_temp_dir("harn-lint-prompt-disabled");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("sample.harn.prompt");
    std::fs::write(&file, "{{ if llm.provider == \"anthropic\" }}x{{ end }}\n").unwrap();
    let outcome = lint_prompt_file_inner(
        &file,
        None,
        &["template-provider-identity-branch".to_string()],
    );
    assert!(!outcome.has_warning);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn collects_prompt_targets_from_directories() {
    let dir = unique_temp_dir("harn-lint-prompt-collect");
    std::fs::create_dir_all(dir.join("nested")).unwrap();
    std::fs::create_dir_all(dir.join("target").join("debug")).unwrap();
    std::fs::create_dir_all(dir.join(".harn").join("cache")).unwrap();
    std::fs::write(dir.join("a.harn.prompt"), "x").unwrap();
    std::fs::write(dir.join("nested").join("b.harn.prompt"), "y").unwrap();
    std::fs::write(dir.join("ignored_by_gitignore.prompt"), "ignored").unwrap();
    std::fs::write(dir.join(".gitignore"), "ignored_by_gitignore.prompt\n").unwrap();
    std::fs::write(
        dir.join("target").join("debug").join("ignored.harn.prompt"),
        "z",
    )
    .unwrap();
    std::fs::write(dir.join(".harn").join("cache").join("ignored.prompt"), "z").unwrap();
    std::fs::write(dir.join("c.txt"), "ignore").unwrap();
    let target = dir.display().to_string();
    let (_harn_files, files) = collect_lint_targets(&[target.as_str()]);
    assert_eq!(files.len(), 2);
    assert!(files.contains(&dir.join("a.harn.prompt")));
    assert!(files.contains(&dir.join("nested").join("b.harn.prompt")));
    assert!(!files.contains(&dir.join("ignored_by_gitignore.prompt")));

    let explicit = dir
        .join("ignored_by_gitignore.prompt")
        .display()
        .to_string();
    let (_harn_files, explicit_files) = collect_lint_targets(&[explicit.as_str()]);
    assert_eq!(
        explicit_files,
        vec![dir.join("ignored_by_gitignore.prompt")]
    );
    let _ = std::fs::remove_dir_all(&dir);
}
