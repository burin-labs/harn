const HOST_DOCUMENT: &str = include_str!("app_host/host.html");
const HOST_STYLE: &str = include_str!("app_host/host.css");
const HOST_PROTOCOL_SCRIPT: &str = include_str!("app_host/protocol.js");
const HOST_SCRIPT: &str = include_str!("app_host/host.js");
const SANDBOX_DOCUMENT: &str = include_str!("app_host/sandbox.html");
const SANDBOX_STYLE: &str = include_str!("app_host/sandbox.css");
const SANDBOX_SCRIPT: &str = include_str!("app_host/sandbox.js");

pub(crate) fn host_document(title: &str, sandbox_origin: &str) -> String {
    let script = format!("{HOST_PROTOCOL_SCRIPT}\n{HOST_SCRIPT}");
    render_document(HOST_DOCUMENT, HOST_STYLE, &script)
        .replace("__HARN_SANDBOX_ORIGIN__", &script_json(sandbox_origin))
        .replace("__HARN_TITLE__", &script_json(title))
        .replace("__HARN_VERSION__", &script_json(env!("CARGO_PKG_VERSION")))
}

pub(crate) fn sandbox_document() -> String {
    render_document(SANDBOX_DOCUMENT, SANDBOX_STYLE, SANDBOX_SCRIPT)
}

fn render_document(template: &str, style: &str, script: &str) -> String {
    debug_assert!(!style.contains("</style"));
    debug_assert!(!script.contains("</script"));
    template
        .replace("__HARN_APP_STYLE__", style)
        .replace("__HARN_APP_SCRIPT__", script)
}

fn script_json(value: &str) -> String {
    serde_json::to_string(value)
        .expect("string serializes")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_supports_the_current_mcp_apps_events() {
        let document = host_document("Test", "http://localhost:4321");
        for required in [
            "serverTools",
            "serverResources",
            "ui/notifications/tool-input",
            "ui/notifications/tool-result",
            "ui/notifications/host-context-changed",
            "ui/resource-teardown",
        ] {
            assert!(document.contains(required), "missing {required}");
        }
        assert!(document.contains(env!("CARGO_PKG_VERSION")));
        assert!(!document.contains("__HARN_"));
    }

    #[test]
    fn sandbox_embeds_checked_source_without_placeholders() {
        let document = sandbox_document();
        assert!(document.contains("sandbox-proxy-ready"));
        assert!(document.contains("sandbox=\"allow-scripts allow-downloads\""));
        assert!(!document.contains("__HARN_"));
    }
}
