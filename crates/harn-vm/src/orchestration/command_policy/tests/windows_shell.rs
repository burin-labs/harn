use super::*;

fn dialect_shell_ctx(command: &str, shell: &str, platform: &str) -> JsonValue {
    serde_json::json!({
        "request": {
            "mode": "shell",
            "command": command,
            "cwd": "/tmp/work",
            "shell": { "id": shell, "platform": platform },
        },
        "workspace_roots": ["/tmp/work"],
    })
}

fn dialect_is_destructive(command: &str, shell: &str) -> bool {
    labels(&command_risk_scan_json(
        &dialect_shell_ctx(command, shell, "windows"),
        None,
    ))
    .contains(&"destructive".to_string())
}

#[test]
fn shell_dialect_registry_normalizes_windows_commands_into_typed_stages() {
    let powershell = security_command_analysis(&dialect_shell_ctx(
        "Write-Output 'C:\\Program Files\\tool.exe'; Remove-Item -Recurse .",
        "pwsh",
        "windows",
    ));
    assert!(
        !powershell.unresolved,
        "static PowerShell must resolve: {powershell:?}"
    );
    assert_eq!(
        powershell.stages[0].argv,
        ["Write-Output", "C:\\Program Files\\tool.exe"]
    );
    assert_eq!(powershell.stages[1].argv, ["Remove-Item", "-Recurse", "."]);

    let cmd = security_command_analysis(&dialect_shell_ctx(
        "echo \"C:\\Program Files\\tool.exe\" & rd /s /q .",
        "cmd.exe",
        "windows",
    ));
    assert!(!cmd.unresolved, "static cmd.exe must resolve: {cmd:?}");
    assert_eq!(cmd.stages[0].argv, ["echo", "C:\\Program Files\\tool.exe"]);
    assert_eq!(cmd.stages[1].argv, ["rd", "/s", "/q", "."]);
}

#[test]
fn typed_shell_allowance_does_not_weaken_process_confinement() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let scan = command_risk_scan_json(
        &serde_json::json!({
            "request": {
                "mode": "shell",
                "command": "Write-Output 'benign'",
                "cwd": workspace.path(),
                "shell": { "id": "pwsh", "platform": "windows" }
            },
            "workspace_roots": [workspace.path()]
        }),
        None,
    );
    assert_eq!(scan["recommended_action"], "allow");

    crate::orchestration::push_execution_policy(crate::orchestration::CapabilityPolicy {
        workspace_roots: vec![workspace.path().display().to_string()],
        sandbox_profile: crate::orchestration::SandboxProfile::Worktree,
        ..Default::default()
    });
    let result = crate::process_sandbox::enforce_process_cwd(outside.path());
    crate::orchestration::pop_execution_policy();
    assert!(
        result.is_err(),
        "a parser allow classification must not grant authority outside the process sandbox"
    );
}

#[test]
fn exact_argv_is_lossless_and_only_actual_shell_wrappers_reparse() {
    let literal = vec![
        "printf".to_string(),
        "%s".to_string(),
        "Remove-Item -Recurse .".to_string(),
        "C:\\Program Files\\tool.exe".to_string(),
    ];
    let analysis = security_command_analysis(&serde_json::json!({
        "request": { "mode": "argv", "argv": literal, "cwd": "/tmp/work" },
        "workspace_roots": ["/tmp/work"],
    }));
    assert_eq!(analysis.stages.len(), 1);
    assert_eq!(analysis.stages[0].argv, literal);
    assert!(!analysis_has_destructive_command(&analysis));

    let wrapped = security_command_analysis(&ctx(&["pwsh", "-Command", "Remove-Item -Recurse ."]));
    assert!(analysis_has_destructive_command(&wrapped));
}

#[test]
fn dynamic_or_unsupported_windows_syntax_is_never_reported_safe() {
    for (shell, command) in [
        ("pwsh", "& $command -Recurse ."),
        ("pwsh", "Remove-Item -Recurse $(Get-Location)"),
        (
            "pwsh",
            "$items | ForEach-Object { Remove-Item -Recurse $_ }",
        ),
        ("cmd.exe", "del /s /q %TARGET%"),
        ("cmd.exe", "for %f in (*) do del %f"),
    ] {
        let scan = command_risk_scan_json(&dialect_shell_ctx(command, shell, "windows"), None);
        assert!(
            labels(&scan).contains(&EXECUTION_SEMANTICS_UNRESOLVED_LABEL.to_string()),
            "dynamic syntax must be unresolved: {shell}: {command} => {scan}"
        );
        assert_ne!(scan["recommended_action"], "allow");
    }
}

#[test]
fn typed_powershell_destructive_fixtures_cover_aliases_nesting_paths_and_encoding() {
    let encoded = powershell_encoded("Remove-Item -Recurse -LiteralPath .");
    for command in [
        "Remove-Item -Recurse '.'".to_string(),
        "ri -Rec -Force .\\*".to_string(),
        "Write-Output before; rm -r -fo .".to_string(),
        "& { Remove-Item -Recurse . }".to_string(),
    ] {
        assert!(
            dialect_is_destructive(&command, "pwsh"),
            "expected typed PowerShell destructive classification: {command}"
        );
    }
    let wrapped = security_command_analysis(&ctx(&["powershell.exe", "-EncodedCommand", &encoded]));
    assert!(analysis_has_destructive_command(&wrapped));
}

#[test]
fn typed_cmd_destructive_fixtures_cover_quoting_chains_and_paths() {
    for command in [
        "rd /s /q \".\"",
        "echo before && del /f /s /q *.*",
        "cmd /c rd /s /q C:\\",
        "format.com D:",
    ] {
        assert!(
            dialect_is_destructive(command, "cmd.exe"),
            "expected typed cmd destructive classification: {command}"
        );
    }
}
