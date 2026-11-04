use super::*;
use chrono::{Duration, NaiveDate};

/// Days a lapsed retirement may stay in the catalog before the sweep fails,
/// so a retirement that lands on a weekend does not open an issue early.
const GRACE_DAYS: i64 = 3;

fn parsed(date: Option<&str>, label: &str, problems: &mut Vec<String>) -> Option<NaiveDate> {
    let date = date?;
    match NaiveDate::parse_from_str(date, "%Y-%m-%d") {
        Ok(date) => Some(date),
        Err(_) => {
            problems.push(format!(
                "{label} has invalid date {date:?}; expected YYYY-MM-DD"
            ));
            None
        }
    }
}

/// A route whose provider has stopped serving it must not stay selectable by
/// alias, and an announced retirement (`retires_on`, kept only while the route
/// is still served) must not outlive its date. Both read structured fields; no
/// date is taken from the free-text note.
fn lapsed_retirements(catalog: &ProviderCatalogArtifact, cutoff: NaiveDate) -> Vec<String> {
    let mut problems = Vec::new();
    for model in &catalog.models {
        if let Some(pricing) = &model.pricing {
            for promotion in &pricing.promotions {
                let label = format!("{} promotion {} review_after", model.id, promotion.id);
                if let Some(review) =
                    parsed(promotion.review_after.as_deref(), &label, &mut problems)
                {
                    if review < cutoff {
                        problems.push(format!(
                            "{} promotion {} needs review after {review}",
                            model.id, promotion.id
                        ));
                    }
                }
            }
        }
        let deprecation = &model.deprecation;
        let sunset = parsed(
            deprecation.sunset_date.as_deref(),
            &format!("{} sunset_date", model.id),
            &mut problems,
        );
        let retires = parsed(
            deprecation.retires_on.as_deref(),
            &format!("{} retires_on", model.id),
            &mut problems,
        );
        if deprecation.status == DeprecationStatus::Deprecated {
            let lapsed = sunset.is_some_and(|date| date < cutoff);
            if lapsed && !model.aliases.is_empty() {
                problems.push(format!(
                    "{} passed its sunset {} but aliases {:?} still select it; remove the aliases",
                    model.id,
                    deprecation.sunset_date.as_deref().unwrap_or_default(),
                    model.aliases
                ));
            }
        } else if let Some(retires) = retires {
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
/// date-based check does not gate a merge: main would redden on a calendar day.
#[test]
fn lapsed_retirements_are_not_left_routable() {
    let Ok(today) = std::env::var("RETIREMENT_SWEEP_DATE") else {
        return;
    };
    let today = NaiveDate::parse_from_str(&today, "%Y-%m-%d")
        .expect("RETIREMENT_SWEEP_DATE must be a YYYY-MM-DD date");
    let cutoff = today - Duration::days(GRACE_DAYS);
    let catalog = artifact_embedded(None, None);
    let dated = catalog
        .models
        .iter()
        .filter(|model| {
            model.deprecation.retires_on.is_some()
                || model.deprecation.sunset_date.is_some()
                || model.pricing.as_ref().is_some_and(|pricing| {
                    pricing
                        .promotions
                        .iter()
                        .any(|promotion| promotion.review_after.is_some())
                })
        })
        .count();
    assert!(dated > 0, "catalog sweep found no dated models");
    let problems = lapsed_retirements(&catalog, cutoff);
    eprintln!(
        "catalog sweep: {dated} dated models, {} needing review",
        problems.len()
    );
    assert!(
        problems.is_empty(),
        "catalog dates needing review:\n{}",
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
fn together_qwen_retirement_keeps_other_provider_route_active() {
    let catalog = artifact_embedded(None, None);
    let together = catalog
        .models
        .iter()
        .find(|model| model.id == "Qwen/Qwen3.8-2.4T-A95B" && model.provider == "together")
        .expect("Together route");
    assert_eq!(together.deprecation.status, DeprecationStatus::Deprecated);
    assert_eq!(
        together.deprecation.sunset_date.as_deref(),
        Some("2026-11-04")
    );
    assert_eq!(
        together.deprecation.superseded_by.as_deref(),
        Some("Qwen/Qwen3.7-Max")
    );
    assert!(together.aliases.is_empty());
    let other = catalog
        .models
        .iter()
        .find(|model| model.id == "qwen/qwen3.8-2.4t-a95b" && model.provider == "openrouter")
        .expect("OpenRouter route");
    assert_eq!(other.deprecation.status, DeprecationStatus::Active);
}

#[test]
fn sol_promotion_review_is_reported_after_its_date() {
    let catalog = artifact_embedded(None, None);
    let mentions_sol = |cutoff| {
        lapsed_retirements(&catalog, cutoff)
            .iter()
            .any(|problem| problem.contains("gpt-5.6-sol promotion openai-2026-08-sol"))
    };
    assert!(!mentions_sol(
        NaiveDate::from_ymd_opt(2026, 11, 30).unwrap()
    ));
    assert!(mentions_sol(NaiveDate::from_ymd_opt(2026, 12, 1).unwrap()));
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

[models."fixture/promotion"]
name = "Promotion"
provider = "openai"
context_window = 8192
pricing = { input_per_mtok = 5.0, output_per_mtok = 30.0, promotions = [{ id = "intro", starts_on = "2025-01-01", review_after = "2026-01-03", input_per_mtok = 4.0, output_per_mtok = 20.0, source_url = "https://example.com" }] }
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
    assert!(
        problems
            .iter()
            .any(|p| p.contains("fixture/promotion promotion intro")),
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
fn malformed_dates_are_reported_by_the_sweep() {
    let overlay = llm_config::parse_config_toml(
        r#"
[models."fixture/bad-sunset"]
name = "Bad sunset"
provider = "openai"
context_window = 8192
deprecated = true
sunset_date = "next month"

[models."fixture/bad-retirement"]
name = "Bad retirement"
provider = "openai"
context_window = 8192
retires_on = "2026-13-01"

[models."fixture/bad-review"]
name = "Bad review"
provider = "openai"
context_window = 8192
pricing = { input_per_mtok = 5.0, output_per_mtok = 30.0, promotions = [{ id = "intro", starts_on = "2025-01-01", review_after = "later", input_per_mtok = 4.0, output_per_mtok = 20.0, source_url = "https://example.com" }] }
"#,
    )
    .expect("overlay parses");
    let catalog = artifact_embedded(Some(&overlay), None);
    let problems = lapsed_retirements(&catalog, NaiveDate::from_ymd_opt(2025, 1, 1).unwrap());
    for field in [
        "fixture/bad-sunset sunset_date",
        "fixture/bad-retirement retires_on",
        "fixture/bad-review promotion intro review_after",
    ] {
        assert!(
            problems
                .iter()
                .any(|problem| problem.starts_with(field) && problem.contains("invalid date")),
            "{field}: {problems:?}"
        );
    }
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
