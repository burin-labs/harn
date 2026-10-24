use super::*;
use chrono::{Duration, NaiveDate, Utc};

/// Days a lapsed retirement may stay in the catalog before the check fails,
/// so a retirement that lands on a weekend does not freeze every merge.
const GRACE_DAYS: i64 = 3;

fn lapsed_before() -> NaiveDate {
    Utc::now().date_naive() - Duration::days(GRACE_DAYS)
}

/// Every `YYYY-MM-DD` date written in free text.
fn dates_in(text: &str) -> Vec<NaiveDate> {
    text.as_bytes()
        .windows(10)
        .filter(|window| {
            window.iter().enumerate().all(|(index, byte)| match index {
                4 | 7 => *byte == b'-',
                _ => byte.is_ascii_digit(),
            })
        })
        .filter_map(|window| std::str::from_utf8(window).ok())
        .filter_map(|candidate| NaiveDate::parse_from_str(candidate, "%Y-%m-%d").ok())
        .collect()
}

/// A route whose provider has stopped serving it must not stay selectable by
/// alias, and a recorded retirement notice must not outlive its date. A row
/// that is only announced carries a dated `deprecation_note` and no
/// `deprecated` flag (a structured sunset excludes the row from selection
/// immediately), so its latest written date is its retirement date.
fn lapsed_retirements(catalog: &ProviderCatalogArtifact, cutoff: NaiveDate) -> Vec<String> {
    let mut problems = Vec::new();
    for model in &catalog.models {
        let deprecation = &model.deprecation;
        if deprecation.status == DeprecationStatus::Deprecated {
            let lapsed = deprecation
                .sunset_date
                .as_deref()
                .and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())
                .is_some_and(|date| date < cutoff);
            if lapsed && !model.aliases.is_empty() {
                problems.push(format!(
                    "{} passed its sunset {} but aliases {:?} still select it; remove the aliases",
                    model.id,
                    deprecation.sunset_date.as_deref().unwrap_or_default(),
                    model.aliases
                ));
            }
        } else if let Some(retires) = deprecation
            .note
            .as_deref()
            .and_then(|note| dates_in(note).into_iter().max())
        {
            if retires < cutoff {
                problems.push(format!(
                    "{} carries a retirement notice dated {retires} but is not deprecated; \
                     set deprecated, sunset_date and superseded_by, or remove the row",
                    model.id
                ));
            }
        }
    }
    problems
}

#[test]
fn lapsed_retirements_are_not_left_routable() {
    let problems = lapsed_retirements(&artifact_embedded(None, None), lapsed_before());
    assert!(
        problems.is_empty(),
        "retirements past their date:\n{}",
        problems.join("\n")
    );
}

#[test]
fn a_lapsed_alias_target_and_a_stale_notice_are_reported() {
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
deprecation_note = "Provider retires this route 2026-01-02 (verified 2025-12-01)."
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
    let early = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
    assert!(lapsed_retirements(&catalog, early)
        .iter()
        .all(|p| !p.starts_with("fixture/")));
}

#[test]
fn retirement_dates_are_read_from_notes() {
    let dates = dates_in("retires 2026-11-07 (notice 2026-10-19, verified 2026-10-21)");
    assert_eq!(dates.len(), 3);
    assert_eq!(
        dates.into_iter().max(),
        NaiveDate::from_ymd_opt(2026, 11, 7)
    );
    assert!(dates_in("no date here, 2026-13-45 is not one").is_empty());
}
