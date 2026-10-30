use super::*;
use chrono::{Duration, NaiveDate};

/// Days a lapsed retirement may stay in the catalog before the sweep fails,
/// so a retirement that lands on a weekend does not open an issue early.
const GRACE_DAYS: i64 = 3;

fn parsed(date: Option<&str>) -> Option<NaiveDate> {
    date.and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())
}

/// A route whose provider has stopped serving it must not stay selectable by
/// alias, and an announced retirement (`retires_on`, kept only while the route
/// is still served) must not outlive its date. Both read structured fields; no
/// date is taken from the free-text note.
fn lapsed_retirements(catalog: &ProviderCatalogArtifact, cutoff: NaiveDate) -> Vec<String> {
    let mut problems = Vec::new();
    for model in &catalog.models {
        let deprecation = &model.deprecation;
        if deprecation.status == DeprecationStatus::Deprecated {
            let lapsed =
                parsed(deprecation.sunset_date.as_deref()).is_some_and(|date| date < cutoff);
            if lapsed && !model.aliases.is_empty() {
                problems.push(format!(
                    "{} passed its sunset {} but aliases {:?} still select it; remove the aliases",
                    model.id,
                    deprecation.sunset_date.as_deref().unwrap_or_default(),
                    model.aliases
                ));
            }
        } else if let Some(retires) = parsed(deprecation.retires_on.as_deref()) {
            if retires < cutoff {
                problems.push(format!(
                    "{} retired on {retires} but is not deprecated; \
                     set deprecated, sunset_date and superseded_by, or remove the row",
                    model.id
                ));
            }
        }
    }
    problems
}

/// The weekly `retirement-sweep` job in provider-contract-probe.yml injects
/// today's date as `RETIREMENT_SWEEP_DATE` and files an issue when this fails.
/// Everywhere else the variable is unset and the test does nothing, because a
/// date-based check must never gate a merge: main would redden on a calendar day.
#[test]
fn lapsed_retirements_are_not_left_routable() {
    let Ok(today) = std::env::var("RETIREMENT_SWEEP_DATE") else {
        return;
    };
    let today = NaiveDate::parse_from_str(&today, "%Y-%m-%d")
        .expect("RETIREMENT_SWEEP_DATE must be a YYYY-MM-DD date");
    let cutoff = today - Duration::days(GRACE_DAYS);
    let problems = lapsed_retirements(&artifact_embedded(None, None), cutoff);
    assert!(
        problems.is_empty(),
        "retirements past their date:\n{}",
        problems.join("\n")
    );
}

#[test]
fn the_embedded_catalog_declares_valid_retirement_fields() {
    let validation = validate_artifact(&artifact_embedded(None, None));
    assert!(
        validation
            .errors
            .iter()
            .all(|error| !error.contains("retires_on") && !error.contains("sunset_date")),
        "{:?}",
        validation.errors
    );
}

#[test]
fn announced_retirements_are_structured_and_recorded() {
    let catalog = artifact_embedded(None, None);
    let inkling = catalog
        .models
        .iter()
        .find(|model| model.id == "together/thinkingmachines/Inkling")
        .expect("the Together Inkling route is cataloged");
    assert_eq!(
        inkling.deprecation.retires_on.as_deref(),
        Some("2026-11-06")
    );
    assert_eq!(inkling.deprecation.status, DeprecationStatus::Active);
    // Negative control: a route with no announced retirement carries no date.
    let silent = catalog
        .models
        .iter()
        .find(|model| {
            model.deprecation.status == DeprecationStatus::Active
                && model.deprecation.retires_on.is_none()
        })
        .expect("some active route has no announced retirement");
    assert!(
        lapsed_retirements(&catalog, NaiveDate::from_ymd_opt(2025, 1, 1).unwrap())
            .iter()
            .all(|problem| !problem.starts_with(&silent.id))
    );
}

#[test]
fn the_explicitly_withdrawn_minimax_free_route_is_retired_without_retiring_live_routes() {
    let catalog = artifact_embedded(None, None);
    let model = |id: &str| {
        catalog
            .models
            .iter()
            .find(|model| model.id == id)
            .unwrap_or_else(|| panic!("missing catalog route {id}"))
    };
    let free = model("minimax/minimax-m3.1-flash:free");
    assert_eq!(free.deprecation.status, DeprecationStatus::Deprecated);
    assert_eq!(free.deprecation.sunset_date.as_deref(), Some("2026-10-22"));
    assert!(free.aliases.is_empty());
    assert!(!catalog
        .routing_routes
        .iter()
        .any(|route| route.provider == "openrouter" && route.model == free.id));
    for id in ["minimax/minimax-m2", "qwen/qwen3-coder-next"] {
        let active = model(id);
        assert_eq!(active.deprecation.status, DeprecationStatus::Active, "{id}");
        assert_eq!(active.deprecation.sunset_date, None, "{id}");
        assert!(catalog
            .routing_routes
            .iter()
            .any(|route| route.provider == "openrouter" && route.model == id));
    }
}

#[test]
fn a_lapsed_alias_target_and_a_lapsed_announcement_are_reported() {
    let overlay = llm_config::parse_config_toml(
        r#"
[models."fixture/gone"]
name = "Gone"
provider = "openai"
context_window = 8192
deprecated = true
deprecation_note = "Retired."
sunset_date = "2026-01-01"

[aliases."fixture-gone"]
id = "fixture/gone"
provider = "openai"

[models."fixture/noticed"]
name = "Noticed"
provider = "openai"
context_window = 8192
deprecation_note = "Provider retires this route; the date is structured."
retires_on = "2026-01-02"

[models."fixture/undated"]
name = "Undated"
provider = "openai"
context_window = 8192
deprecation_note = "Notice received 2025-12-01 announces an undated upgrade."
"#,
    )
    .expect("overlay parses");
    let catalog = artifact_embedded(Some(&overlay), None);
    let cutoff = NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
    let problems = lapsed_retirements(&catalog, cutoff);
    assert!(
        problems.iter().any(|p| p.starts_with("fixture/gone ")),
        "{problems:?}"
    );
    assert!(
        problems.iter().any(|p| p.starts_with("fixture/noticed ")),
        "{problems:?}"
    );
    // A date written only in prose is context, never a retirement.
    assert!(
        problems.iter().all(|p| !p.starts_with("fixture/undated ")),
        "{problems:?}"
    );
    let early = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
    assert!(lapsed_retirements(&catalog, early)
        .iter()
        .all(|p| !p.starts_with("fixture/")));
}

#[test]
fn retires_on_is_validated_as_a_date_on_active_rows_only() {
    let overlay = llm_config::parse_config_toml(
        r#"
[models."fixture/badly-dated"]
name = "Badly dated"
provider = "openai"
context_window = 8192
retires_on = "next month"

[models."fixture/doubled"]
name = "Doubled"
provider = "openai"
context_window = 8192
deprecated = true
deprecation_note = "Retired."
sunset_date = "2026-01-01"
retires_on = "2026-01-01"
"#,
    )
    .expect("overlay parses");
    let errors = validate_artifact(&artifact_embedded(Some(&overlay), None)).errors;
    assert!(
        errors
            .iter()
            .any(|e| e.contains("fixture/badly-dated") && e.contains("retires_on must be an ISO")),
        "{errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("fixture/doubled") && e.contains("already deprecated")),
        "{errors:?}"
    );
}
