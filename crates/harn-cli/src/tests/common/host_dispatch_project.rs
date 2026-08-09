use std::path::{Path, PathBuf};

/// Write a bounded project whose manifest installs one trigger handler.
///
/// Callers supply the handler source so each CLI transport can exercise its
/// own privileged graph shape without duplicating manifest discovery setup.
pub fn write_host_dispatch_trigger_project(
    root: &Path,
    declared: bool,
    trigger_handler_source: &str,
) -> PathBuf {
    // Stop manifest discovery at this fixture rather than allowing an ancestor
    // checkout to decide whether the project is privileged.
    std::fs::create_dir_all(root.join(".git")).expect("project boundary");
    let check_section = if declared {
        "\n[check]\ntrusted_host_dispatch = true\n"
    } else {
        ""
    };
    std::fs::write(
        root.join("harn.toml"),
        format!(
            r#"
[package]
name = "host-dispatch-cli-fixture"

[exports]
trigger_handlers = "trigger_handlers.harn"

[[triggers]]
id = "cron-handler"
kind = "cron"
provider = "cron"
schedule = "* * * * *"
match = {{ events = ["cron.tick"] }}
handler = "trigger_handlers::on_tick"
{check_section}"#
        ),
    )
    .expect("write manifest");
    std::fs::write(root.join("trigger_handlers.harn"), trigger_handler_source)
        .expect("write trigger handler");
    let script = root.join("main.harn");
    std::fs::write(
        &script,
        r#"
pipeline main(harness: Harness) {
  harness.stdio.println("target-ran")
}
"#,
    )
    .expect("write script");
    script
}
