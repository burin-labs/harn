use super::*;
use tempfile::TempDir;

fn fixture(manifest: &str, mcp: Option<&str>) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("plugin.json"), manifest).unwrap();
    if let Some(mcp) = mcp {
        fs::write(dir.path().join("mcp.json"), mcp).unwrap();
    }
    dir
}

fn manifest(extra: &str) -> String {
    format!(r#"{{"$schema":"{MANIFEST_SCHEMA}","name":"acme.tools"{extra}}}"#)
}

fn mcp(servers: &str) -> String {
    format!(r#"{{"$schema":"{MCP_SCHEMA}","mcpServers":{{{servers}}}}}"#)
}

#[test]
fn loads_stdio_and_http_into_runtime_specs() {
    let root = fixture(
        &manifest(""),
        Some(&mcp(
            r#""local":{"type":"stdio","command":"node","args":["${PLUGIN_ROOT}/server.js","${PLUGIN_DATA}"],"env":{"MODE":"${PLUGIN_ROOT}"}},"remote":{"type":"streamable-http","url":"https://example.com/mcp","headers":{"X-Tenant":"acme"}}"#,
        )),
    );
    let data = root.path().join("data");
    let report = load_agent_plugin(root.path(), &data);
    assert!(
        report.accepted && report.conformant,
        "{:?}",
        report.diagnostics
    );
    assert_eq!(report.runtime_specs().len(), 2);
    let plugin = report.plugin.unwrap();
    let PluginMcpServer::Stdio { args, env, .. } = &plugin.mcp_servers["local"] else {
        panic!()
    };
    assert_eq!(args[1], plugin.data_dir.to_string_lossy());
    assert_eq!(env["PLUGIN_ROOT"], plugin.root.to_string_lossy());
}

#[test]
fn fatal_manifest_rejects_whole_plugin_but_unknown_field_is_nonfatal() {
    let bad = fixture(&manifest(",\"extra\":true"), None);
    let report = load_agent_plugin(bad.path(), bad.path().join("data"));
    assert!(report.accepted && !report.conformant);
    assert_eq!(report.diagnostics[0].code, "AP_MANIFEST_UNKNOWN_FIELD");
    let fatal = fixture(&manifest(",\"author\":\"nope\""), None);
    let report = load_agent_plugin(fatal.path(), fatal.path().join("data"));
    assert!(!report.accepted && report.plugin.is_none());
    assert!(report.diagnostics.iter().any(|item| item.fatal));
}

#[test]
fn invalid_mcp_entry_isolated_from_sibling() {
    let root = fixture(
        &manifest(""),
        Some(&mcp(
            r#""bad":{"type":"stdio","command":"sh -c bad"},"good":{"type":"stdio","command":"node"}"#,
        )),
    );
    let report = load_agent_plugin(root.path(), root.path().join("data"));
    assert!(report.accepted && !report.conformant);
    assert_eq!(
        report
            .plugin
            .unwrap()
            .mcp_servers
            .keys()
            .collect::<Vec<_>>(),
        vec!["good"]
    );
    assert_eq!(report.diagnostics[0].scope, DiagnosticScope::McpServer);
}

#[test]
fn expansion_is_nonrecursive_and_reserved_env_is_rejected() {
    let root = fixture(
        &manifest(""),
        Some(&mcp(
            r#""expand":{"type":"stdio","command":"node","args":["${PLUGIN_ROOT}/${PLUGIN_DATA}"]},"reserved":{"type":"stdio","command":"node","env":{"PLUGIN_ROOT":"bad"}}"#,
        )),
    );
    let data = PathBuf::from("/tmp/${PLUGIN_ROOT}");
    let report = load_agent_plugin(root.path(), &data);
    let plugin = report.plugin.unwrap();
    let PluginMcpServer::Stdio { args, .. } = &plugin.mcp_servers["expand"] else {
        panic!()
    };
    assert!(
        args[0].contains("${PLUGIN_ROOT}"),
        "expansion recursed: {}",
        args[0]
    );
    assert!(!plugin.mcp_servers.contains_key("reserved"));
}

#[test]
fn remote_urls_and_case_folded_headers_fail_closed_per_server() {
    let root = fixture(
        &manifest(""),
        Some(&mcp(
            r#""plain":{"type":"streamable-http","url":"http://example.com/mcp"},"dupe":{"type":"streamable-http","url":"https://example.com/mcp","headers":{"X-A":"1","x-a":"2"}},"local":{"type":"streamable-http","url":"http://127.0.0.1:3000/mcp"}"#,
        )),
    );
    let report = load_agent_plugin(root.path(), root.path().join("data"));
    assert_eq!(
        report
            .plugin
            .unwrap()
            .mcp_servers
            .keys()
            .collect::<Vec<_>>(),
        vec!["local"]
    );
    assert_eq!(report.diagnostics.len(), 2);
}

#[test]
fn invalid_mcp_file_does_not_disable_manifest() {
    let root = fixture(&manifest(""), Some(r#"{"mcpServers":{}}"#));
    let report = load_agent_plugin(root.path(), root.path().join("data"));
    assert!(report.accepted && report.plugin.as_ref().unwrap().mcp_servers.is_empty());
    assert_eq!(report.diagnostics[0].scope, DiagnosticScope::Mcp);
}

#[test]
fn data_root_may_be_created_after_validation() {
    let root = fixture(
        &manifest(""),
        Some(&mcp(
            r#""local":{"type":"stdio","command":"node","cwd":"${PLUGIN_DATA}/work"}"#,
        )),
    );
    let data = root.path().join("not-created-yet");
    let report = load_agent_plugin(root.path(), &data);
    assert!(report.conformant, "{:?}", report.diagnostics);
    assert!(!data.exists(), "inspection must remain read-only");
    let plugin = report.plugin.unwrap();
    plugin.prepare_runtime_specs().unwrap();
    assert!(
        plugin.data_dir.is_dir(),
        "launch preparation creates PLUGIN_DATA"
    );
}

#[test]
fn valid_but_unsupported_sse_has_a_distinct_diagnostic() {
    let root = fixture(
        &manifest(""),
        Some(&mcp(
            r#""legacy":{"type":"sse","url":"https://example.com/events"}"#,
        )),
    );
    let report = load_agent_plugin(root.path(), root.path().join("data"));
    assert_eq!(report.diagnostics[0].code, "AP_MCP_TRANSPORT_UNSUPPORTED");
}

#[test]
fn validates_agent_skills_format_without_harn_only_short_field() {
    let root = fixture(&manifest(""), None);
    let skill = root.path().join("skills").join("review");
    fs::create_dir_all(&skill).unwrap();
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: review\ndescription: Review changes when a patch needs feedback.\nmetadata:\n  owner: acme\n---\n# Review\n",
    )
    .unwrap();
    let report = load_agent_plugin(root.path(), root.path().join("data"));
    assert!(report.conformant, "{:?}", report.diagnostics);
    let skill = &report.plugin.unwrap().skills[0];
    assert_eq!(skill.name, "review");
    assert!(skill.description.starts_with("Review changes"));
}

#[test]
fn invalid_agent_skill_is_skipped_without_hiding_valid_sibling() {
    let root = fixture(&manifest(""), None);
    let skills = root.path().join("skills");
    for (directory, frontmatter) in [
        (
            "good",
            "name: good\ndescription: A valid portable skill for tests.",
        ),
        (
            "bad",
            "name: bad\ndescription: A bad skill.\nshort: harn-only field",
        ),
    ] {
        let path = skills.join(directory);
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join("SKILL.md"),
            format!("---\n{frontmatter}\n---\n# Body\n"),
        )
        .unwrap();
    }
    let report = load_agent_plugin(root.path(), root.path().join("data"));
    assert_eq!(report.plugin.unwrap().skills[0].name, "good");
    assert_eq!(report.diagnostics[0].code, "AP_SKILL_INVALID");
}

#[cfg(unix)]
#[test]
fn symlinked_command_cannot_escape_plugin_root() {
    use std::os::unix::fs::symlink;
    let root = fixture(
        &manifest(""),
        Some(&mcp(r#""bad":{"type":"stdio","command":"./escape"}"#)),
    );
    symlink("/bin/sh", root.path().join("escape")).unwrap();
    let report = load_agent_plugin(root.path(), root.path().join("data"));
    assert!(report.plugin.unwrap().mcp_servers.is_empty());
    assert!(report.diagnostics[0].message.contains("symlink"));
}

#[cfg(unix)]
#[test]
fn symlinked_fixed_components_obey_their_failure_boundaries() {
    use std::os::unix::fs::symlink;
    let outside = tempfile::tempdir().unwrap();
    fs::write(
        outside.path().join("mcp.json"),
        mcp(r#""outside":{"type":"stdio","command":"node"}"#),
    )
    .unwrap();
    fs::create_dir(outside.path().join("skills")).unwrap();

    let root = fixture(&manifest(""), None);
    symlink(
        outside.path().join("mcp.json"),
        root.path().join("mcp.json"),
    )
    .unwrap();
    symlink(outside.path().join("skills"), root.path().join("skills")).unwrap();
    let report = load_agent_plugin(root.path(), root.path().join("data"));
    let plugin = report.plugin.unwrap();
    assert!(plugin.skills.is_empty() && plugin.mcp_servers.is_empty());
    assert_eq!(report.diagnostics.len(), 2);
    assert!(report.diagnostics.iter().all(|item| !item.fatal));

    let manifest_root = tempfile::tempdir().unwrap();
    symlink(
        outside.path().join("mcp.json"),
        manifest_root.path().join("plugin.json"),
    )
    .unwrap();
    let report = load_agent_plugin(manifest_root.path(), manifest_root.path().join("data"));
    assert!(!report.accepted && report.diagnostics[0].fatal);
}

#[cfg(unix)]
#[test]
fn dangling_fixed_location_is_present_and_invalid() {
    use std::os::unix::fs::symlink;
    let root = fixture(&manifest(""), None);
    symlink("missing-skills", root.path().join("skills")).unwrap();
    symlink("missing-mcp.json", root.path().join("mcp.json")).unwrap();
    let report = load_agent_plugin(root.path(), root.path().join("data"));
    assert_eq!(report.diagnostics.len(), 2);
    assert!(report
        .diagnostics
        .iter()
        .any(|item| item.code == "AP_SKILLS_NOT_DIRECTORY"));
    assert!(report
        .diagnostics
        .iter()
        .any(|item| item.code == "AP_MCP_NOT_FILE"));
}
