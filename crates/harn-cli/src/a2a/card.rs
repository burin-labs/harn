use std::path::Path;

/// Generate the Agent Card JSON for a pipeline file (v1.0 schema).
pub(super) fn agent_card(pipeline_name: &str, port: u16) -> serde_json::Value {
    serde_json::json!({
        "id": pipeline_name,
        "name": pipeline_name,
        "description": "Harn pipeline agent",
        "url": format!("http://localhost:{port}"),
        "version": env!("CARGO_PKG_VERSION"),
        "provider": {
            "organization": "Harn",
            "url": "https://harn.dev"
        },
        "interfaces": [
            {"protocol": "jsonrpc", "url": "/"}
        ],
        "securitySchemes": [],
        "capabilities": {
            "streaming": true,
            "pushNotifications": false,
            "extendedAgentCard": false
        },
        "skills": [
            {
                "id": "execute",
                "name": "Execute Pipeline",
                "description": "Run the harn pipeline with a task"
            }
        ]
    })
}

/// Extract the pipeline name from a .harn file path (stem without extension).
pub(super) fn pipeline_name_from_path(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("default")
        .to_string()
}
