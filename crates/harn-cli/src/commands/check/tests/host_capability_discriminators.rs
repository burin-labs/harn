use super::*;
use crate::commands::check::host_capabilities::parse_host_capability_value;

#[test]
fn parse_host_capability_value_accepts_top_level_object_schema() {
    let value = serde_json::json!({
        "workspace": ["project_root", "file_exists"],
        "runtime": { "operations": ["task", "pipeline_input"] },
        "harn_cloud": {
            "operations": {
                "agent_api": {
                    "param_discriminators": {
                        "operation": ["agents.get", "agents.list"],
                        "forwarded_operation": {
                            "values": ["agents.get", "agents.list"],
                            "allow_dynamic": true
                        }
                    }
                }
            }
        }
    });
    let parsed = parse_host_capability_value(&value);
    assert!(parsed.contains_operation("workspace", "project_root"));
    assert!(parsed.contains_operation("workspace", "file_exists"));
    assert!(parsed.contains_operation("runtime", "task"));
    assert!(parsed.contains_operation("runtime", "pipeline_input"));
    assert!(parsed.contains_operation("harn_cloud", "agent_api"));
    assert_eq!(
        parsed
            .param_discriminators("harn_cloud", "agent_api")
            .unwrap()["operation"]
            .allowed_values,
        ["agents.get".to_string(), "agents.list".to_string()]
            .into_iter()
            .collect()
    );
    let forwarded = &parsed
        .param_discriminators("harn_cloud", "agent_api")
        .unwrap()["forwarded_operation"];
    assert_eq!(forwarded.allowed_values.len(), 2);
    assert!(forwarded.allow_dynamic);
    let projected = parsed.into_manifest_entries();
    assert_eq!(
        projected["harn_cloud"]["operations"]["agent_api"]["param_discriminators"]
            ["forwarded_operation"]["allow_dynamic"],
        true
    );
    assert_eq!(
        projected["harn_cloud"]["operations"]["agent_api"]["param_discriminators"]["operation"]
            ["values"][0],
        "agents.get"
    );
}

#[test]
fn preflight_validates_host_param_discriminator_literals() {
    let dir = unique_temp_dir("harn-check-host-param-discriminator");
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = dir.join("host-capabilities.json");
    std::fs::write(
        &manifest,
        r#"{
          "capabilities": {
            "harn_cloud": {
              "operations": {
                "agent_api": {
                  "param_discriminators": {
                    "operation": ["agents.get", "agents.list"]
                  }
                }
              }
            }
          }
        }"#,
    )
    .unwrap();
    let config = CheckConfig {
        host: harn_modules::host_capability_config::HostCapabilityConfig {
            host_capabilities_path: Some(manifest.display().to_string()),
            ..Default::default()
        },
        ..CheckConfig::default()
    };

    let accepted = r#"
pipeline main() {
  host_call("harn_cloud.agent_api", {operation: "agents.get", params: {}})
}
"#;
    let file = dir.join("accepted.harn");
    let diagnostics =
        collect_preflight_diagnostics(&file, accepted, &parse_program(accepted), &config);
    assert!(
        diagnostics.is_empty(),
        "declared discriminator should pass: {:?}",
        diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );

    let misspelled = r#"
pipeline main() {
  host_call("harn_cloud.agent_api", {operation: "agents.geet", params: {}})
}
"#;
    let diagnostics = collect_preflight_diagnostics(
        &dir.join("misspelled.harn"),
        misspelled,
        &parse_program(misspelled),
        &config,
    );
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == harn_parser::DiagnosticCode::CapabilityUnknownOperation
            && diagnostic.message.contains("agents.geet")
            && diagnostic.message.contains("`operation` discriminator")
    }));

    let dynamic = r#"
pipeline main(op) {
  host_call("harn_cloud.agent_api", {operation: op, params: {}})
}
"#;
    let diagnostics = collect_preflight_diagnostics(
        &dir.join("dynamic.harn"),
        dynamic,
        &parse_program(dynamic),
        &config,
    );
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == harn_parser::DiagnosticCode::CapabilityCallStaticNameRequired
            && diagnostic
                .message
                .contains("requires a literal `operation` params field")
    }));

    let forwarding_manifest = dir.join("forwarding-host-capabilities.json");
    std::fs::write(
        &forwarding_manifest,
        r#"{
          "capabilities": {
            "harn_cloud": {
              "operations": {
                "agent_api": {
                  "param_discriminators": {
                    "operation": {
                      "values": ["agents.get", "agents.list"],
                      "allow_dynamic": true
                    }
                  }
                }
              }
            }
          }
        }"#,
    )
    .unwrap();
    let forwarding_config = CheckConfig {
        host: harn_modules::host_capability_config::HostCapabilityConfig {
            host_capabilities_path: Some(forwarding_manifest.display().to_string()),
            ..Default::default()
        },
        ..CheckConfig::default()
    };
    let diagnostics = collect_preflight_diagnostics(
        &dir.join("forwarding.harn"),
        dynamic,
        &parse_program(dynamic),
        &forwarding_config,
    );
    assert!(
        diagnostics.is_empty(),
        "manifest opted into a separately validated forwarding boundary: {:?}",
        diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}
