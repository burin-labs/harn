use super::*;

#[test]
fn default_report_includes_core_recommendations() {
    let report = build_report(Path::new(DEFAULT_NOTES_PATH), &[]).expect("report");
    for id in [
        "anthropic",
        "openai",
        "gemini",
        "mistral",
        "ollama",
        "local",
    ] {
        assert!(
            report.providers.iter().any(|entry| entry.id == id),
            "missing provider support entry {id}"
        );
    }
    let mistral = report
        .providers
        .iter()
        .find(|entry| entry.id == "mistral")
        .expect("mistral row");
    assert_eq!(mistral.catalog_provider, "openrouter");
    assert_eq!(mistral.recommended.model, "mistralai/mistral-small-2603");
    assert_eq!(mistral.empirical.status, "not_recorded");

    let azure = report
        .providers
        .iter()
        .find(|entry| entry.id == "azure_openai")
        .expect("azure row");
    assert_ne!(azure.recommended.model, "*");
    assert!(azure.capabilities.native_tools);
    assert!(
        azure.capabilities.batch_api,
        "Azure OpenAI should surface Batch API support"
    );

    let openai = report
        .providers
        .iter()
        .find(|entry| entry.id == "openai")
        .expect("openai row");
    assert_eq!(openai.recommended.model, "gpt-5.4-mini");
    assert!(
        openai
            .capabilities
            .serving_tiers
            .iter()
            .any(|tier| tier.id == "flex" && tier.economics == "discounted"),
        "OpenAI support row should surface synchronous Flex separately from Batch"
    );

    let gemini = report
        .providers
        .iter()
        .find(|entry| entry.id == "gemini")
        .expect("gemini row");
    assert!(
        gemini
            .capabilities
            .serving_tiers
            .iter()
            .any(|tier| tier.id == "priority" && tier.request_value.as_deref() == Some("priority")),
        "Gemini support row should surface Priority as a synchronous serving tier"
    );
}

/// A provider with chat routes must name its recommendation, so adding a
/// cheaper row can never move it. Dropping Fireworks' pin (it has no
/// quality-check default to fall back on) must fail generation and name
/// Fireworks; xAI, which has no note entry but does have a quality-check
/// default, must not be named.
#[test]
fn provider_with_chat_routes_and_no_chosen_recommendation_fails() {
    let mut notes: toml::Value = toml::from_str(EMBEDDED_NOTES_TOML).expect("notes toml");
    let entries = notes
        .get_mut("entry")
        .and_then(toml::Value::as_array_mut)
        .expect("entry array");
    let fireworks = entries
        .iter_mut()
        .find(|entry| entry.get("id").and_then(toml::Value::as_str) == Some("fireworks"))
        .and_then(toml::Value::as_table_mut)
        .expect("fireworks note entry");
    assert!(
        fireworks.remove("recommended_model").is_some(),
        "fireworks note must pin a model for this control to mean anything"
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("notes.toml");
    fs::write(&path, toml::to_string(&notes).expect("render notes")).expect("write notes");

    let error = build_report(&path, &[]).expect_err("unpinned provider must fail");
    assert!(error.contains("fireworks"), "{error}");
    assert!(!error.contains("xai"), "{error}");

    build_report(Path::new(DEFAULT_NOTES_PATH), &[]).expect("shipped notes pin every provider");
}

#[test]
fn curated_recommendations_do_not_point_at_superseded_models() {
    let report = build_report(Path::new(DEFAULT_NOTES_PATH), &[]).expect("report");
    for entry in &report.providers {
        if entry.recommended.model == "*" || entry.recommended.model.contains('*') {
            continue;
        }
        let model = harn_vm::llm_config::model_catalog_entry(&entry.recommended.model)
            .unwrap_or_else(|| panic!("{} recommends missing catalog model", entry.id));
        assert_eq!(
            model.provider, entry.catalog_provider,
            "{} recommends {} from provider {}, not {}",
            entry.id, entry.recommended.model, model.provider, entry.catalog_provider
        );
        assert!(
            model.superseded_by.is_none(),
            "{} recommends superseded model {} -> {:?}",
            entry.id,
            entry.recommended.model,
            model.superseded_by
        );
    }
}

/// A recommended route is a route a reader sends a prompt to, so no
/// provider may recommend a row that does not serve text generation, and a
/// provider whose whole catalog is decision-only must recommend nothing at
/// all rather than fall back to a capability-rule match pattern.
///
/// The number of text-free providers measured is asserted non-zero, and
/// TypeSafe is named explicitly, so a catalog that stopped shipping a
/// decision-only provider fails here instead of passing vacuously.
#[test]
fn no_provider_recommends_a_route_that_cannot_answer_a_prompt() {
    let report = build_report(Path::new(DEFAULT_NOTES_PATH), &[]).expect("report");
    let mut decision_only_providers = 0_usize;
    for entry in &report.providers {
        if entry.recommended.model == "*" || entry.recommended.model.contains('*') {
            continue;
        }
        let model = harn_vm::llm_config::model_catalog_entry(&entry.recommended.model)
            .unwrap_or_else(|| panic!("{} recommends missing catalog model", entry.id));
        assert!(
            model.supports_operation(harn_vm::llm_config::ModelOperation::TextGeneration),
            "{} recommends {}, which does not serve text generation",
            entry.id,
            entry.recommended.model
        );
    }
    let catalog = harn_vm::llm_config::model_catalog_entries();
    for entry in &report.providers {
        let rows: Vec<_> = catalog
            .iter()
            .filter(|(_, model)| model.provider == entry.catalog_provider)
            .collect();
        // A provider with no cataloged rows at all (Azure OpenAI, for one)
        // has made no operation claim, and its recommendation legitimately
        // comes from a capability match pattern. The rule here is about a
        // provider whose rows exist and all refuse text.
        if rows.is_empty()
            || rows.iter().any(|(_, model)| {
                model.supports_operation(harn_vm::llm_config::ModelOperation::TextGeneration)
            })
        {
            continue;
        }
        decision_only_providers += 1;
        assert_eq!(
            entry.recommended.model, "*",
            "{} serves no text route but recommends {}",
            entry.id, entry.recommended.model
        );
    }
    assert!(
        decision_only_providers > 0,
        "no provider in the catalog is text-free, so this test measured nothing"
    );
    // The concrete case this rule was written for, named so a future
    // catalog that drops every text-free provider fails here loudly
    // instead of leaving the rule measuring only empty providers.
    let typesafe = report
        .providers
        .iter()
        .find(|entry| entry.catalog_provider == "typesafe")
        .expect("typesafe support row");
    assert_eq!(typesafe.recommended.model, "*");
    assert_eq!(typesafe.recommended.display_name, None);
}

#[test]
fn empirical_summary_attaches_to_matching_model() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let summary = tmp.path().join("summary.json");
    fs::write(
        &summary,
        r#"{
          "runs": [
            {
              "run_id": "python-add__openrouter_mistral-small__native",
              "selector": {"provider": "openrouter", "model": "mistralai/mistral-small-2603"},
              "tool_format": "native",
              "status": "passed",
              "passed": true,
              "skipped": false
            },
            {
              "run_id": "python-add__openrouter_mistral-small__text",
              "selector": {"provider": "openrouter", "model": "mistralai/mistral-small-2603"},
              "tool_format": "text",
              "status": "failed",
              "passed": false,
              "skipped": false
            }
          ],
          "comparisons": [
            {
              "selector": {"provider": "openrouter", "model": "mistralai/mistral-small-2603"},
              "equivalent": false
            }
          ]
        }"#,
    )
    .expect("write summary");

    let report = build_report(Path::new(DEFAULT_NOTES_PATH), &[summary]).expect("report");
    let mistral = report
        .providers
        .iter()
        .find(|entry| entry.id == "mistral")
        .expect("mistral row");
    assert_eq!(mistral.empirical.total_runs, 2);
    assert_eq!(mistral.empirical.passed_runs, 1);
    assert_eq!(
        mistral.empirical.best_tool_format.as_deref(),
        Some("native")
    );
    assert_eq!(
        mistral.empirical.native_text_parity.as_deref(),
        Some("diverged")
    );
    assert_eq!(mistral.empirical.sources, vec!["summary.json"]);

    let openrouter = report
        .providers
        .iter()
        .find(|entry| entry.id == "openrouter")
        .expect("openrouter row");
    assert_eq!(openrouter.empirical.status, "not_recorded");
}

#[test]
fn empirical_summary_matches_model_patterns() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let summary = tmp.path().join("summary.json");
    fs::write(
        &summary,
        r#"{
          "runs": [
            {
              "run_id": "azure-gpt4o-native",
              "selector": {"provider": "azure_openai", "model": "gpt-4o"},
              "tool_format": "native",
              "status": "passed",
              "passed": true,
              "skipped": false
            }
          ]
        }"#,
    )
    .expect("write summary");

    let report = build_report(Path::new(DEFAULT_NOTES_PATH), &[summary]).expect("report");
    let azure = report
        .providers
        .iter()
        .find(|entry| entry.id == "azure_openai")
        .expect("azure row");
    assert_eq!(azure.recommended.model, "gpt-*");
    assert_eq!(azure.empirical.status, "observed_pass");
    assert_eq!(azure.empirical.total_runs, 1);
}

#[test]
fn markdown_and_json_render_generated_surfaces() {
    let report = build_report(Path::new(DEFAULT_NOTES_PATH), &[]).expect("report");
    let markdown = render_markdown(&report);
    assert!(markdown.contains("GENERATED by `harn provider catalog support`"));
    assert!(markdown.contains("Provider support recommendations"));
    assert!(!markdown.contains("API_KEY="));

    let json = render_json(&report).expect("json");
    let parsed: JsonValue = serde_json::from_str(&json).expect("valid json");
    assert_eq!(parsed["schema_version"], PROVIDER_SUPPORT_SCHEMA_VERSION);
    assert!(parsed["providers"]
        .as_array()
        .is_some_and(|rows| rows.len() >= 6));
}
