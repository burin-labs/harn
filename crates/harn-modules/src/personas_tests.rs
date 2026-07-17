use super::*;

fn context(names: &[&str]) -> PersonaValidationContext {
    PersonaValidationContext {
        known_capabilities: default_persona_capabilities(),
        known_tools: BTreeSet::from(["github".to_string(), "ci".to_string()]),
        known_names: names.iter().map(|name| name.to_string()).collect(),
    }
}

#[test]
fn validates_sample_manifest() {
    let parsed = parse_persona_manifest_str(
        r#"
[[personas]]
name = "merge_captain"
description = "Owns PR readiness."
entry_workflow = "workflows/merge_captain.harn#run"
tools = ["github", "ci"]
capabilities = ["git.get_diff"]
autonomy = "act_with_approval"
receipts = "required"
triggers = ["github.pr_opened"]
schedules = ["*/30 * * * *"]
handoffs = ["review_captain"]
context_packs = ["repo_policy"]
evals = ["merge_safety"]
budget = { daily_usd = 20.0 }

[[personas]]
name = "review_captain"
description = "Reviews code."
entry_workflow = "workflows/review_captain.harn#run"
tools = ["github"]
autonomy_tier = "suggest"
receipt_policy = "optional"
"#,
    )
    .expect("manifest parses");

    validate_persona_manifests(
        Path::new("harn.toml"),
        &parsed.personas,
        &context(&["merge_captain", "review_captain"]),
    )
    .expect("manifest validates");
}

#[test]
fn parses_output_style_as_string_and_table() {
    let parsed = parse_persona_manifest_str(
        r#"
[[personas]]
name = "concise_bot"
description = "Terse."
entry_workflow = "workflows/x.harn#run"
tools = ["github"]
autonomy = "suggest"
receipts = "optional"
output_style = "concise"

[[personas]]
name = "styled_bot"
description = "Styled."
entry_workflow = "workflows/y.harn#run"
tools = ["github"]
autonomy = "suggest"
receipts = "optional"
output_style = { name = "friendly", instructions = "Use warm, plain language." }
"#,
    )
    .expect("manifest parses");

    let first = parsed.personas[0].output_style.as_ref().expect("style set");
    assert_eq!(first.name.as_deref(), Some("concise"));
    assert_eq!(first.instructions, None);

    let second = parsed.personas[1].output_style.as_ref().expect("style set");
    assert_eq!(second.name.as_deref(), Some("friendly"));
    assert_eq!(
        second.instructions.as_deref(),
        Some("Use warm, plain language.")
    );

    // A persona without the field leaves it None and validates fine.
    validate_persona_manifests(
        Path::new("harn.toml"),
        &parsed.personas,
        &context(&["concise_bot", "styled_bot"]),
    )
    .expect("manifest validates");
}

#[test]
fn bad_manifest_produces_typed_errors() {
    let parsed = parse_persona_manifest_str(
        r#"
[[personas]]
name = "bad"
description = ""
entry_workflow = ""
tools = ["unknown"]
capabilities = ["git"]
autonomy = "shadow"
receipts = "required"
triggers = ["github"]
schedules = [""]
handoffs = ["missing"]
budget = { daily_usd = -1.0, surprise = true }
surprise = true
"#,
    )
    .expect("manifest parses");

    let errors =
        validate_persona_manifests(Path::new("harn.toml"), &parsed.personas, &context(&["bad"]))
            .expect_err("manifest rejects");
    let fields: BTreeSet<_> = errors
        .iter()
        .map(|error| error.field_path.as_str())
        .collect();
    assert!(fields.contains("[[personas]][0].description"));
    assert!(fields.contains("[[personas]][0].entry_workflow"));
    assert!(fields.contains("[[personas]][0].tools"));
    assert!(fields.contains("[[personas]][0].capabilities"));
    assert!(fields.contains("[[personas]][0].triggers"));
    assert!(fields.contains("[[personas]][0].schedules"));
    assert!(fields.contains("[[personas]][0].handoffs"));
    assert!(fields.contains("[[personas]][0].budget.daily_usd"));
    assert!(fields.contains("[[personas]][0].budget.surprise"));
    assert!(fields.contains("[[personas]][0].surprise"));
}

#[test]
fn manifest_stages_round_trip_through_serde() {
    let parsed = parse_persona_manifest_str(
        r#"
[[personas]]
name = "scoped"
description = "Per-stage scoping demo."
entry_workflow = "workflows/scoped.harn#run"
tools = ["github", "ci"]
autonomy = "act_with_approval"
receipts = "required"

[[personas.stages]]
name = "research"
allowed_tools = ["github"]
side_effect_level = "read_only"

[[personas.stages]]
name = "act"
allowed_tools = ["github", "ci"]
side_effect_level = "process_exec"
max_iterations = 4
on_exit = { on_complete = "research" }
"#,
    )
    .expect("manifest parses");

    validate_persona_manifests(
        Path::new("harn.toml"),
        &parsed.personas,
        &context(&["scoped"]),
    )
    .expect("stage-scoped manifest validates");
    let persona = &parsed.personas[0];
    assert_eq!(persona.stages.len(), 2);
    assert_eq!(persona.stages[0].name, "research");
    assert_eq!(
        persona.stages[0].allowed_tools.as_deref(),
        Some(["github".to_string()].as_slice())
    );
    assert_eq!(
        persona.stages[1]
            .on_exit
            .as_ref()
            .unwrap()
            .on_complete
            .as_deref(),
        Some("research")
    );

    // Round-trip via the TOML serializer to ensure the shape is stable.
    let serialised = toml::to_string(&PersonaManifestDocument {
        personas: parsed.personas.clone(),
    })
    .expect("serialize");
    let reparsed = parse_persona_manifest_str(&serialised).expect("reparse");
    assert_eq!(reparsed.personas, parsed.personas);
}

#[test]
fn stage_validation_flags_unknown_targets_and_levels() {
    let parsed = parse_persona_manifest_str(
        r#"
[[personas]]
name = "scoped"
description = "Bad stages."
entry_workflow = "workflows/scoped.harn#run"
tools = ["github"]
autonomy = "suggest"
receipts = "optional"

[[personas.stages]]
name = "research"
allowed_tools = ["ci"]
side_effect_level = "do_anything"
on_exit = { on_complete = "missing" }

[[personas.stages]]
name = "research"
"#,
    )
    .expect("manifest parses");

    let errors = validate_persona_manifests(
        Path::new("harn.toml"),
        &parsed.personas,
        &context(&["scoped"]),
    )
    .expect_err("rejects bad stage config");
    let fields: BTreeSet<_> = errors
        .iter()
        .map(|error| error.field_path.as_str())
        .collect();
    assert!(fields.contains("[[personas]][0].stages[0].allowed_tools"));
    assert!(fields.contains("[[personas]][0].stages[0].side_effect_level"));
    assert!(fields.contains("[[personas]][0].stages[0].on_exit.on_complete"));
    assert!(fields.contains("[[personas]][0].stages[1].name"));
}

#[test]
fn source_persona_picks_up_stage_attributes() {
    let parsed = parse_persona_source_str(
        r#"
@persona(name: "scoped", tools: [github, ci], stages: [
  {name: "research", allowed_tools: [github]},
  {name: "act", allowed_tools: [github, ci], side_effect_level: "process_exec"},
])
fn scoped(ctx) {
  research(ctx)
  act(ctx)
}

@step(name: "research") fn research(ctx) { return ctx }
@step(name: "act") fn act(ctx) { return ctx }
"#,
    )
    .expect("source persona parses");

    let persona = &parsed.personas[0];
    assert_eq!(persona.stages.len(), 2);
    assert_eq!(persona.stages[0].name, "research");
    assert_eq!(
        persona.stages[0].allowed_tools.as_deref(),
        Some(["github".to_string()].as_slice()),
    );
    assert_eq!(
        persona.stages[1].side_effect_level.as_deref(),
        Some("process_exec"),
    );
}

#[test]
fn source_persona_extracts_called_steps_in_order() {
    let parsed = parse_persona_source_str(
        r#"
@persona(name: "merge_captain")
fn merge_captain(ctx) {
  plan(ctx)
  verify(ctx)
}

@step(name: "plan", model: "gpt-5.4-mini", retry: {max_attempts: 2})
fn plan(ctx) {
  return ctx
}

@step(name: "verify", error_boundary: continue)
fn verify(ctx) {
  return ctx
}
"#,
    )
    .expect("source persona parses");
    assert_eq!(parsed.personas.len(), 1);
    let persona = &parsed.personas[0];
    assert_eq!(persona.name.as_deref(), Some("merge_captain"));
    assert_eq!(persona.steps.len(), 2);
    assert_eq!(persona.steps[0].name, "plan");
    assert_eq!(persona.steps[0].model.as_deref(), Some("gpt-5.4-mini"));
    assert_eq!(persona.steps[0].retry.as_ref().unwrap().max_attempts, 2);
    assert_eq!(persona.steps[1].error_boundary.as_deref(), Some("continue"));
}
