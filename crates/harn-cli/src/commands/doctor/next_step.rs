use super::{DoctorCheck, DoctorStatus};

pub(super) fn next_step_suggestion(checks: &[DoctorCheck]) -> String {
    let creds_ok = checks
        .iter()
        .any(|check| check.id == "creds:any" && check.status == DoctorStatus::Ok);
    let ollama = checks.iter().find(|check| check.id == "ollama");
    let manifest_present = checks
        .iter()
        .any(|check| check.label == "manifest" && check.status == DoctorStatus::Ok);
    let any_fail = checks
        .iter()
        .any(|check| check.status == DoctorStatus::Fail);
    let no_ollama_models = matches!(
        ollama.map(|check| check.status),
        Some(DoctorStatus::Skip) | Some(DoctorStatus::Warn) | None
    );

    if !creds_ok && no_ollama_models {
        return "Run `harn models recommend` to pick a starter model for your machine.".to_string();
    }
    if ollama.is_some_and(|check| check.status == DoctorStatus::Warn) {
        return "Run `harn models recommend` to pick a starter model for your machine.".to_string();
    }
    if creds_ok && !manifest_present {
        return "Run `harn new my-agent --template agent` to scaffold a project.".to_string();
    }
    if !any_fail {
        return "You're ready. Try `harn try \"summarize this README\"` or `harn demo` for a bundled scenario."
            .to_string();
    }
    "Address the failing checks above, then re-run `harn doctor`.".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(id: &str, status: DoctorStatus) -> DoctorCheck {
        DoctorCheck {
            id: id.to_string(),
            status,
            label: id.to_string(),
            ..DoctorCheck::default()
        }
    }

    #[test]
    fn suggestions_match_complete_commands() {
        let cases = [
            (
                vec![
                    check("creds:any", DoctorStatus::Fail),
                    check("ollama", DoctorStatus::Skip),
                ],
                "Run `harn models recommend` to pick a starter model for your machine.",
            ),
            (
                vec![
                    check("creds:any", DoctorStatus::Ok),
                    check("ollama", DoctorStatus::Ok),
                ],
                "Run `harn new my-agent --template agent` to scaffold a project.",
            ),
            (
                vec![
                    check("creds:any", DoctorStatus::Ok),
                    check("ollama", DoctorStatus::Ok),
                    check("manifest", DoctorStatus::Ok),
                ],
                "You're ready. Try `harn try \"summarize this README\"` or `harn demo` for a bundled scenario.",
            ),
        ];

        for (checks, expected) in cases {
            assert_eq!(next_step_suggestion(&checks), expected);
        }
    }
}
