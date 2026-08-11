//! Plan-level capability tests: what the solver decides is required, before
//! any edit is emitted.

use super::*;

#[test]
fn capability_apply_preserves_mcp_tool_harness_and_narrows_private_helpers() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("assistant.mcp.harn");
    let original = "pub fn search(harness: Harness) {\n  return harness.net.get(\"https://example.com\")\n}\n\nfn timestamp(harness: Harness) {\n  return harness.clock.now_ms()\n}\n";
    fs::write(&script, original).unwrap();

    super::apply_repairs_with_options_at(
        temp.path(),
        RepairSafety::SurfaceChanging,
        false,
        super::FixOptions::capability_migrations(),
    )
    .unwrap();

    let updated = fs::read_to_string(&script).unwrap();
    assert!(
        updated.contains("pub fn search(harness: Harness)"),
        "the MCP runtime supplies a root Harness to public tools:\n{updated}"
    );
    assert!(
        updated.contains("fn timestamp(clock: HarnessClock)"),
        "ordinary private helpers should still attenuate:\n{updated}"
    );
}

/// A capability reached only through `${...}` is still reached.
///
/// `visit::walk_program` stops at `Node::InterpolatedString`: the lexer keeps a
/// hole as unparsed source text, so it has no AST children. The whole-program
/// solver computed required capabilities with that walk, so a handle used only
/// inside a hole looked unused. The repair then deleted it from the parameter
/// type and every call site while the interpolation kept calling it. `harn fix
/// --apply` runs unattended in the fleet bump workflow, so it rewrote working
/// helpers into code that does not compile and shipped that as a bump PR
/// (harn-cloud#1469).
///
/// The fixture needs THREE capabilities. With two, no attenuation repair is
/// proposed at all and the test passes vacuously — which is exactly how an
/// earlier version of it passed against the unfixed solver.
///
/// The assertion is EQUIVALENCE, not merely "did not delete": the same use
/// inside and outside a hole must plan the same repair. "Did not delete" alone
/// would also hold if the analysis simply gave up on any file containing a hole.
#[test]
fn capability_plan_counts_uses_inside_string_interpolation() {
    let plan_edits = |name: &str, body: &str| {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join(name);
        fs::write(
            &script,
            format!(
                "fn with_temp_dir(harness: {{fs: HarnessFs, process: HarnessProcess, random: HarnessRandom}}, body) {{\n\
                 {body}\
                 \x20 harness.fs.mkdir(dir)\n\
                 \x20 const outcome = try {{\n\
                 \x20   body(dir)\n\
                 \x20 }}\n\
                 \x20 harness.process.run({{program: \"rm\", args: [\"-rf\", dir]}})\n\
                 \x20 return unwrap(outcome)\n\
                 }}\n\n\
                 pipeline main(harness: Harness, task) {{\n\
                 \x20 return with_temp_dir(\n\
                 \x20   {{fs: harness.fs, process: harness.process, random: harness.random}},\n\
                 \x20   {{ dir -> dir }},\n\
                 \x20 )\n\
                 }}\n"
            ),
        )
        .unwrap();
        // `--apply` is a different entry from `build_plan`; only this one
        // reaches the repair that broke. Apply for real and read the file back,
        // so the assertion is on emitted code rather than on a plan.
        super::apply_repairs_with_options_at(
            temp.path(),
            RepairSafety::SurfaceChanging,
            false,
            super::FixOptions::capability_migrations(),
        )
        .unwrap();
        fs::read_to_string(&script).unwrap()
    };

    // Identical programs; `random` is reached through a hole in one and through
    // a plain statement in the other.
    let through_hole = plan_edits(
        "hole.harn",
        "  const dir = \".tmp-${harness.random.uuid_v7()}\"\n",
    );
    let through_statement = plan_edits(
        "statement.harn",
        "  const id = harness.random.uuid_v7()\n  const dir = \".tmp-${id}\"\n",
    );

    let signature = |source: &str| {
        source
            .lines()
            .find(|line| line.starts_with("fn with_temp_dir"))
            .unwrap_or_default()
            .to_string()
    };

    assert!(
        !through_hole.contains("harness.random")
            || signature(&through_hole).contains("HarnessRandom")
            || signature(&through_hole).contains("harness: Harness"),
        "the body still calls `harness.random`, so the repair must not have removed it \
         from the signature:\n{through_hole}"
    );
    assert_eq!(
        signature(&through_hole),
        signature(&through_statement),
        "a capability reached through `${{...}}` must be repaired the same as one \
         reached through a statement"
    );
}

#[test]
fn capability_plan_repairs_imported_helpers_without_type_diagnostics() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("main.harn");
    fs::write(
        &script,
        "import { agent_capture_events } from \"std/agent/events\"\nimport { agent_parse_tool_calls } from \"std/agent/primitives\"\nimport { agent_session_finalize, agent_session_messages, agent_reminder_providers_fire } from \"std/agent/state\"\n\nfn custom_agent(harness: Harness) -> HarnessAgent {\n  return harness.agent\n}\n\npipeline main(harness: Harness, task) {\n  const session = \"session\"\n  const messages = agent_session_messages(session)\n  agent_session_finalize(session, \"done\")\n  agent_session_finalize(custom_agent(harness), session, \"already explicit\")\n  const captured = agent_capture_events(session, fn() { nil })\n  const parsed = agent_parse_tool_calls(\"<tool_call>x({})</tool_call>\", [], \"text\")\n  const report = agent_reminder_providers_fire(session, \"session_idle\", {}, {})\n  return {messages: messages, captured: captured, parsed: parsed, report: report}\n}\n",
    )
    .unwrap();
    let files = vec![script.clone()];
    let graph = commands::check::build_module_graph(&files);

    let repairs = whole_program_capabilities::plan(
        &files,
        &graph,
        &[],
        &BTreeSet::new(),
        &Default::default(),
        &mut Vec::new(),
    )
    .unwrap();

    assert_eq!(
        repairs
            .iter()
            .flat_map(|repair| &repair.edits)
            .filter(|edit| {
                edit.span.start == edit.span.end && edit.replacement == "harness.agent, "
            })
            .count(),
        5,
        "every imported Agent helper must derive its prefix from the module signature: {repairs:#?}"
    );

    let mut updated = fs::read_to_string(&script).unwrap();
    let mut edits = repairs
        .iter()
        .flat_map(|repair| repair.edits.iter().cloned())
        .collect::<Vec<_>>();
    edits.sort_by_key(|edit| std::cmp::Reverse((edit.span.start, edit.span.end)));
    for edit in edits {
        updated.replace_range(edit.span.start..edit.span.end, &edit.replacement);
    }
    fs::write(&script, updated).unwrap();
    let repaired_graph = commands::check::build_module_graph(&files);
    let fixed_point = whole_program_capabilities::plan(
        &files,
        &repaired_graph,
        &[],
        &BTreeSet::new(),
        &Default::default(),
        &mut Vec::new(),
    )
    .unwrap();
    assert!(
        fixed_point.is_empty(),
        "already-migrated imported calls must be a planner fixed point: {fixed_point:#?}"
    );
}

#[test]
fn capability_plan_preserves_explicit_imported_capability_expressions() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("main.harn");
    fs::write(
        &script,
        "import { terminal_width } from \"std/tui\"\n\nfn custom_term(term: HarnessTerm) -> HarnessTerm {\n  return term\n}\n\npipeline main(harness: Harness, task) {\n  const term = custom_term(harness.term)\n  return [terminal_width(custom_term(harness.term)), terminal_width(term)]\n}\n",
    )
    .unwrap();
    let files = vec![script];
    let graph = commands::check::build_module_graph(&files);

    let repairs = whole_program_capabilities::plan(
        &files,
        &graph,
        &[],
        &BTreeSet::new(),
        &Default::default(),
        &mut Vec::new(),
    )
    .unwrap();

    assert!(
        repairs.is_empty(),
        "explicit capability expressions must not be shifted into optional ordinary parameters: {repairs:#?}"
    );
}

#[test]
fn capability_plan_preserves_an_unknown_capability_identifier_when_ordinary_args_are_missing() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("main.harn");
    fs::write(
        &script,
        "import { agent_session_messages } from \"std/agent/state\"\n\nfn custom_agent(harness: Harness) -> HarnessAgent {\n  return harness.agent\n}\n\npipeline main(harness: Harness, task) {\n  const agent = custom_agent(harness)\n  return agent_session_messages(agent)\n}\n",
    )
    .unwrap();
    let files = vec![script];
    let graph = commands::check::build_module_graph(&files);

    let repairs = whole_program_capabilities::plan(
        &files,
        &graph,
        &[],
        &BTreeSet::new(),
        &Default::default(),
        &mut Vec::new(),
    )
    .unwrap();

    assert!(
        !repairs
            .iter()
            .flat_map(|repair| &repair.edits)
            .any(|edit| edit.replacement == "harness.agent, "),
        "an unknown identifier may already be the carrier; adding one would shift it into the missing session slot: {repairs:#?}"
    );
}

#[test]
fn capability_plan_uses_inferred_capability_types_to_repair_a_different_missing_carrier() {
    let temp = tempfile::TempDir::new().unwrap();
    let library = temp.path().join("library.harn");
    let entrypoint = temp.path().join("main.harn");
    fs::write(
        &library,
        "pub fn run_with_root(harness: Harness, agent: HarnessAgent) {\n  return {root: harness, agent: agent}\n}\n",
    )
    .unwrap();
    fs::write(
        &entrypoint,
        "import { run_with_root } from \"./library\"\n\nfn custom_agent(harness: Harness) -> HarnessAgent {\n  return harness.agent\n}\n\npipeline main(harness: Harness, task) {\n  const agent = custom_agent(harness)\n  return run_with_root(agent)\n}\n",
    )
    .unwrap();
    let files = vec![entrypoint, library];
    let graph = commands::check::build_module_graph(&files);

    let repairs = whole_program_capabilities::plan(
        &files,
        &graph,
        &[],
        &BTreeSet::new(),
        &Default::default(),
        &mut Vec::new(),
    )
    .unwrap();

    assert!(
        repairs
            .iter()
            .flat_map(|repair| &repair.edits)
            .any(|edit| edit.replacement == "harness, "),
        "an inferred Agent cannot occupy a root Harness slot, so the missing root carrier is observable: {repairs:#?}"
    );
}

#[test]
fn capability_plan_disambiguates_shadowed_inferred_bindings_by_declaration() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("main.harn");
    fs::write(
        &script,
        "import { agent_session_messages } from \"std/agent/state\"\n\nfn custom_agent(harness: Harness) -> HarnessAgent {\n  return harness.agent\n}\n\npipeline main(harness: Harness, task) {\n  const value = custom_agent(harness)\n  const before = agent_session_messages(value, \"outer-before\")\n  const nested = [1].map({ _ ->\n    const value = \"nested-session\"\n    return agent_session_messages(value)\n  })\n  const after = agent_session_messages(value, \"outer-after\")\n  return {before: before, nested: nested, after: after}\n}\n",
    )
    .unwrap();
    let files = vec![script];
    let graph = commands::check::build_module_graph(&files);

    let repairs = whole_program_capabilities::plan(
        &files,
        &graph,
        &[],
        &BTreeSet::new(),
        &Default::default(),
        &mut Vec::new(),
    )
    .unwrap();

    assert_eq!(
        repairs
            .iter()
            .flat_map(|repair| &repair.edits)
            .filter(|edit| edit.replacement == "harness.agent, ")
            .count(),
        1,
        "only the string-valued shadow should receive the missing Agent carrier: {repairs:#?}"
    );
}

#[test]
fn capability_plan_completes_a_partial_imported_capability_prefix() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("main.harn");
    fs::write(
        &script,
        "import { which } from \"std/os\"\n\npipeline main(harness: Harness, task) {\n  return which(harness.tools, \"git\")\n}\n",
    )
    .unwrap();
    let files = vec![script.clone()];
    let graph = commands::check::build_module_graph(&files);

    let repairs = whole_program_capabilities::plan(
        &files,
        &graph,
        &[],
        &BTreeSet::new(),
        &Default::default(),
        &mut Vec::new(),
    )
    .unwrap();
    let edits = repairs
        .iter()
        .flat_map(|repair| &repair.edits)
        .filter(|edit| edit.replacement == ", harness.system")
        .collect::<Vec<_>>();
    assert_eq!(
        edits.len(),
        1,
        "only the missing capability suffix should be inserted: {repairs:#?}"
    );
    let on_disk = fs::read_to_string(&script).unwrap();
    #[expect(clippy::string_slice, reason = "test input is ASCII")]
    let inserted_at = &on_disk[edits[0].span.start..edits[0].span.end];
    assert_eq!(inserted_at, "");

    let mut updated = fs::read_to_string(&script).unwrap();
    let mut all_edits = repairs
        .iter()
        .flat_map(|repair| repair.edits.iter().cloned())
        .collect::<Vec<_>>();
    all_edits.sort_by_key(|edit| std::cmp::Reverse((edit.span.start, edit.span.end)));
    for edit in all_edits {
        updated.replace_range(edit.span.start..edit.span.end, &edit.replacement);
    }
    assert_eq!(
        call_argument_paths(&updated, "which")[0],
        [
            Some("harness.tools".to_string()),
            Some("harness.system".to_string()),
            None,
        ]
    );
    fs::write(&script, updated).unwrap();
    let repaired_graph = commands::check::build_module_graph(&files);
    assert!(
        whole_program_capabilities::plan(
            &files,
            &repaired_graph,
            &[],
            &BTreeSet::new(),
            &Default::default(),
            &mut Vec::new(),
        )
        .unwrap()
        .is_empty(),
        "a completed imported prefix must be a planner fixed point"
    );
}

#[test]
fn capability_plan_resolves_private_imported_capability_aliases() {
    let temp = tempfile::TempDir::new().unwrap();
    let library = temp.path().join("library.harn");
    let entrypoint = temp.path().join("main.harn");
    fs::write(
        &library,
        "type AgentHandle = HarnessAgent\n\npub fn imported_helper(agent: AgentHandle, session: string) {\n  return agent.snapshot(session)\n}\n",
    )
    .unwrap();
    fs::write(
        &entrypoint,
        "import { imported_helper } from \"./library\"\n\npipeline main(harness: Harness, task) {\n  return imported_helper(\"session\")\n}\n",
    )
    .unwrap();
    let files = vec![entrypoint, library];
    let graph = commands::check::build_module_graph(&files);

    let repairs = whole_program_capabilities::plan(
        &files,
        &graph,
        &[],
        &BTreeSet::new(),
        &Default::default(),
        &mut Vec::new(),
    )
    .unwrap();

    assert!(
        repairs.iter().flat_map(|repair| &repair.edits).any(|edit| {
            edit.span.start == edit.span.end && edit.replacement == "harness.agent, "
        }),
        "a private signature alias must resolve through the module graph: {repairs:#?}"
    );
}
