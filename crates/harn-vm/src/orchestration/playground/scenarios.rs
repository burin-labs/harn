//! Built-in scenario manifests for the Merge Captain mock-repos
//! playground (#1020). These seed manifests are embedded from the
//! crate source tree so published crates are self-contained and
//! `harn merge-captain mock init --scenario <name>` works without
//! requiring an examples/ checkout at runtime.

use std::path::Path;

use crate::value::VmError;

use super::manifest::ScenarioManifest;

const THREE_REPO_BASIC: &str = include_str!("scenarios/three_repo_basic.json");
const SINGLE_GREEN: &str = include_str!("scenarios/single_green.json");
const FORCE_PUSH_DRILL: &str = include_str!("scenarios/force_push_drill.json");

pub fn builtin_scenario_names() -> Vec<&'static str> {
    vec!["three_repo_basic", "single_green", "force_push_drill"]
}

pub fn load_builtin(name: &str) -> Result<ScenarioManifest, VmError> {
    let raw = match name {
        "three_repo_basic" => THREE_REPO_BASIC,
        "single_green" => SINGLE_GREEN,
        "force_push_drill" => FORCE_PUSH_DRILL,
        other => {
            return Err(VmError::Runtime(format!(
                "unknown built-in scenario {other:?} (available: {})",
                builtin_scenario_names().join(", ")
            )))
        }
    };
    ScenarioManifest::parse(raw.as_bytes(), Path::new(&format!("<builtin:{name}>")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_parses_and_validates() {
        for name in builtin_scenario_names() {
            load_builtin(name).unwrap_or_else(|e| panic!("scenario {name}: {e}"));
        }
    }

    #[test]
    fn embedded_scenarios_match_workspace_examples_when_available() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace_examples = manifest_dir.join("../../examples/merge_captain/scenarios");
        if !workspace_examples.exists() {
            return;
        }

        for (name, embedded) in [
            ("three_repo_basic", THREE_REPO_BASIC),
            ("single_green", SINGLE_GREEN),
            ("force_push_drill", FORCE_PUSH_DRILL),
        ] {
            let source = std::fs::read_to_string(workspace_examples.join(format!("{name}.json")))
                .unwrap_or_else(|e| panic!("failed to read workspace scenario {name}: {e}"));
            assert_eq!(
                embedded, source,
                "embedded scenario {name} drifted from examples/merge_captain/scenarios"
            );
        }
    }
}
