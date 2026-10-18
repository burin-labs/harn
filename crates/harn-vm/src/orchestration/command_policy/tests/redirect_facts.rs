use super::*;

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
}
