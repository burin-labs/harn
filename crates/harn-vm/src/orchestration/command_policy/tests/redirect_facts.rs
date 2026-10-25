use super::*;

#[test]
fn deterministic_scan_preserves_binding_only_and_outer_shell_source_stages() {
    let scan = command_risk_scan_json(
        &shell_ctx("PATH='/older git/bin'; HOME=/other sh -c 'git history'"),
        None,
    );
    assert_eq!(
        scan["shell_source_groups"],
        serde_json::json!([
            [{"argv": [], "source_argv": ["PATH=/older git/bin"]}],
            [{"argv": ["sh", "-c", "git history"],
              "source_argv": ["HOME=/other", "sh", "-c", "git history"]}]
        ])
    );
    assert_eq!(scan["shell_command_groups"].as_array().unwrap().len(), 1);
    assert_eq!(
        scan["execution_stages"][0]["argv"],
        serde_json::json!(["git", "history"])
    );
    let literal = command_risk_scan_json(
        &shell_ctx("git commit -m 'PATH=/older/bin; HOME=/other'"),
        None,
    );
    let argv = serde_json::json!(["git", "commit", "-m", "PATH=/older/bin; HOME=/other"]);
    assert_eq!(literal["shell_source_groups"][0][0]["argv"], argv);
    assert_eq!(literal["shell_source_groups"][0][0]["source_argv"], argv);
    assert_eq!(
        command_risk_scan_json(&shell_ctx(""), None)["shell_source_groups"],
        serde_json::json!([])
    );
    let literal_argv = command_risk_scan_json(&ctx(&["printf", "%s", "PATH=x; git history"]), None);
    assert!(literal_argv["shell_source_groups"].is_null());
    for shell in ["cmd", "pwsh", "unsupported-shell"] {
        let mut context = shell_ctx("PATH=x; git history");
        context["request"]["shell"]["id"] = serde_json::json!(shell);
        assert!(
            command_risk_scan_json(&context, None)["shell_source_groups"].is_null(),
            "{shell}"
        );
    }
}

#[test]
fn deterministic_scan_projects_structural_shell_redirects() {
    for context in [
        shell_ctx("node -e 'console.log(Number(process.env.LIMIT) > 1)'"),
        shell_ctx(r"printf '%s' left\>right"),
        ctx(&["printf", "%s", "left > right"]),
    ] {
        let scan = command_risk_scan_json(&context, None);
        assert_eq!(scan["shell_redirects"], serde_json::json!([]));
        assert!(scan["execution_semantics"]["unresolved_reason"].is_null());
        assert_eq!(scan["shell_io_complete"], true);
    }
    for context in [
        shell_ctx("cat .env > 'credential copy'"),
        shell_ctx("sh -c \"cat .env > 'credential copy'\""),
        shell_ctx("printf '%s' \"$(cat .env > 'credential copy')\""),
        ctx(&["sh", "-c", "cat .env > 'credential copy'"]),
    ] {
        let scan = command_risk_scan_json(&context, None);
        assert_eq!(
            scan["shell_redirects"],
            serde_json::json!([{
                "operator": ">", "destination": "credential copy",
                "dynamic": false, "writes_file": true,
            }]),
            "{context}"
        );
    }
    let dynamic = command_risk_scan_json(&shell_ctx("cat .env > \"$DESTINATION\""), None);
    assert_eq!(dynamic["shell_redirects"][0]["dynamic"], true);
    assert_eq!(dynamic["shell_redirects"][0]["writes_file"], true);
    let program = "console.log(1 > process.env.LIMIT)";
    let code = command_risk_scan_json(
        &shell_ctx(&format!("node -e '{program}' > config.env")),
        None,
    );
    assert_eq!(
        code["execution_stages"][0]["argv"],
        serde_json::json!(["node", "-e", program])
    );
    let nested = command_risk_scan_json(&shell_ctx("sh -c 'cat .env > destination'"), None);
    assert_eq!(
        nested["execution_stages"][0]["argv"],
        serde_json::json!(["cat", ".env"])
    );
    let input = command_risk_scan_json(&shell_ctx("cat < .env > destination"), None);
    assert_eq!(input["shell_io_complete"], true);
    assert_eq!(input["shell_redirects"][0]["operator"], "<");
    assert_eq!(input["shell_redirects"][0]["destination"], ".env");
    for context in [shell_ctx("env | tee destination"), ctx(&["env"])] {
        let environment = command_risk_scan_json(&context, None);
        assert_eq!(environment["shell_io_complete"], true);
        assert_eq!(
            environment["execution_stages"][0]["source_argv"],
            serde_json::json!(["env"])
        );
    }
    let prefixed = command_risk_scan_json(
        &shell_ctx("env API_KEY=$OPENAI_API_KEY node script.js > destination"),
        None,
    );
    assert_eq!(
        prefixed["execution_stages"][0]["argv"],
        serde_json::json!(["node", "script.js"])
    );
    assert_eq!(
        prefixed["execution_stages"][0]["source_argv"],
        serde_json::json!(["env", "API_KEY=$OPENAI_API_KEY", "node", "script.js"])
    );
    let assigned = command_risk_scan_json(
        &shell_ctx("API_KEY=$OPENAI_API_KEY node script.js > destination"),
        None,
    );
    assert_eq!(
        assigned["execution_stages"][0]["source_argv"],
        serde_json::json!(["API_KEY=$OPENAI_API_KEY", "node", "script.js"])
    );
    let descriptor = command_risk_scan_json(&shell_ctx("printf safe 2>&1"), None);
    assert_eq!(descriptor["shell_redirects"], serde_json::json!([]));
    let sink = command_risk_scan_json(&shell_ctx("printf safe > /dev/null"), None);
    assert_eq!(sink["shell_redirects"][0]["writes_file"], true);
    assert_eq!(sink["shell_redirects"][0]["destination"], "/dev/null");
    let mut unsupported = shell_ctx("cat .env > destination");
    unsupported["request"]["shell"]["id"] = serde_json::json!("unsupported-shell");
    let unresolved = command_risk_scan_json(&unsupported, None);
    assert_eq!(unresolved["shell_redirects"], serde_json::json!([]));
    assert_eq!(
        unresolved["execution_semantics"]["unresolved_reason"],
        "shell_dialect_unknown"
    );
    assert_eq!(unresolved["shell_io_complete"], false);
    let heredoc = command_risk_scan_json(
        &shell_ctx("cat > output <<EOF\n$OPENAI_API_KEY\nEOF\n"),
        None,
    );
    assert_eq!(heredoc["shell_io_complete"], false);
    let herestring =
        command_risk_scan_json(&shell_ctx("cat <<<\"$OPENAI_API_KEY\" > output"), None);
    assert_eq!(herestring["shell_io_complete"], false);
    for shell in ["cmd", "pwsh"] {
        let mut context = shell_ctx("echo safe > output");
        context["request"]["shell"]["id"] = serde_json::json!(shell);
        assert_eq!(
            command_risk_scan_json(&context, None)["shell_io_complete"],
            false
        );
    }
    for text in [
        "printf %s $(printf 'left right') > result.txt",
        "printf %s $((1 + 2)) > result.txt",
    ] {
        let lossy = command_risk_scan_json(&shell_ctx(text), None);
        assert_eq!(lossy["shell_redirects"][0]["destination"], "result.txt");
        assert_eq!(
            lossy["execution_stages"][0]["argv"],
            serde_json::json!(["printf", "%s"])
        );
        assert_eq!(lossy["shell_io_complete"], false, "{text}");
    }
    let literal = command_risk_scan_json(&shell_ctx("printf %s 'left right' > result.txt"), None);
    assert_eq!(literal["shell_io_complete"], true);
    assert_eq!(
        literal["execution_stages"][0]["source_argv"],
        serde_json::json!(["printf", "%s", "left right"])
    );
}
