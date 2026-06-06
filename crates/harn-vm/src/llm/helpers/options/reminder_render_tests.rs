use super::*;
use super::{reminders::*, system_prompt::*};

fn reminder(role_hint: ReminderRoleHint, body: &str) -> SystemReminder {
    SystemReminder {
        id: "reminder-1".to_string(),
        tags: vec!["test".to_string()],
        dedupe_key: None,
        ttl_turns: None,
        preserve_on_compact: false,
        propagate: crate::llm::helpers::ReminderPropagate::Session,
        role_hint,
        source: crate::llm::helpers::ReminderSource::InPipeline,
        body: body.to_string(),
        fired_at_turn: 0,
        originating_agent_id: None,
    }
}

#[test]
fn anthropic_user_block_renders_as_xml_user_content_block() {
    crate::llm::capabilities::clear_user_overrides();
    let caps = crate::llm::capabilities::lookup("mock", "claude-sonnet-4-7");
    let rendered = render_pending_reminders(
        &caps,
        &[reminder(
            ReminderRoleHint::EphemeralCache,
            "remember <this>",
        )],
    );

    let RenderedReminder::Message(message) = &rendered[0] else {
        panic!("anthropic user block should render as a message");
    };
    assert_eq!(message["role"], "user");
    assert!(message["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("<system-reminder>"));
    assert!(message["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("remember &lt;this&gt;"));
    assert_eq!(
        message["content"][0]["cache_control"],
        serde_json::json!({"type": "ephemeral"})
    );
}

#[test]
fn openai_developer_capability_renders_separate_developer_message() {
    crate::llm::capabilities::clear_user_overrides();
    let caps = crate::llm::capabilities::lookup("mock", "o3");
    let rendered = render_pending_reminders(
        &caps,
        &[reminder(ReminderRoleHint::System, "keep policy in mind")],
    );

    let RenderedReminder::Message(message) = &rendered[0] else {
        panic!("OpenAI developer route should render as a message");
    };
    assert_eq!(message["role"], "developer");
    assert_eq!(
        message["content"].as_str(),
        Some("System reminder:\nkeep policy in mind")
    );
}

#[test]
fn gemini_xml_capability_renders_system_text_with_xml_scaffolding() {
    crate::llm::capabilities::clear_user_overrides();
    let caps = crate::llm::capabilities::lookup("gemini", "gemini-2.5-flash");
    let rendered =
        render_pending_reminders(&caps, &[reminder(ReminderRoleHint::System, "use context")]);

    let RenderedReminder::SystemText(text) = &rendered[0] else {
        panic!("Gemini route should fold reminder into system text");
    };
    assert_eq!(text, "<system-reminder>\nuse context\n</system-reminder>");
}

#[test]
fn local_fallback_renders_plain_system_text() {
    let caps = crate::llm::capabilities::Capabilities::default();
    let rendered = render_pending_reminders(&caps, &[reminder(ReminderRoleHint::System, "plain")]);

    assert_eq!(
        rendered,
        vec![RenderedReminder::SystemText(
            "System reminder:\nplain".to_string()
        )]
    );
}

#[test]
fn system_text_reminders_are_excluded_from_system_string() {
    let options = BTreeMap::from([
        (
            "system_prompt_parts".to_string(),
            VmValue::String(std::sync::Arc::from("parts")),
        ),
        (
            "system_appendix".to_string(),
            VmValue::String(std::sync::Arc::from("appendix")),
        ),
    ]);
    // A `SystemText` reminder must NOT appear in the assembled `system`
    // string — it is routed to a trailing message instead so the system
    // string stays cache-stable across turns.
    let prompt = compose_system_prompt_with_reminders(
        Some("base".to_string()),
        Some(&options),
        &[RenderedReminder::SystemText("reminder".to_string())],
    )
    .expect("system prompt")
    .expect("non-empty prompt");
    assert_eq!(prompt, "parts\n\nbase\n\nappendix");

    // The reminder text instead lands as the trailing user message.
    let messages = apply_rendered_reminder_messages(
        vec![serde_json::json!({"role": "assistant", "content": "ok"})],
        &[RenderedReminder::SystemText("reminder".to_string())],
    );
    let last = messages.last().expect("trailing message");
    assert_eq!(last["role"], "user");
    assert_eq!(last["content"], "reminder");
}

fn s(text: &str) -> VmValue {
    VmValue::String(std::sync::Arc::from(text))
}

fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
    VmValue::Dict(std::sync::Arc::new(
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
    ))
}

fn list(items: Vec<VmValue>) -> VmValue {
    VmValue::List(std::sync::Arc::new(items))
}

#[test]
fn full_host_option_ordering_is_faithful() {
    let options = BTreeMap::from([
        ("system_preamble".to_string(), s("P")),
        ("system_prefix".to_string(), s("X")),
        ("system_context".to_string(), s("C")),
        ("system_prompt_parts".to_string(), s("parts")),
        ("system_appendix".to_string(), s("A")),
        ("system_suffix".to_string(), s("S")),
    ]);
    let prompt = compose_system_prompt_with_reminders(
        Some("base".to_string()),
        Some(&options),
        &[RenderedReminder::SystemText("R".to_string())],
    )
    .expect("system prompt")
    .expect("non-empty prompt");
    // Before bucket in declaration order (preamble, prefix, context,
    // parts, primary), then After bucket (appendix, suffix). The
    // `SystemText` reminder ("R") is no longer folded into the system
    // string — it is appended as a trailing message instead.
    assert_eq!(prompt, "P\n\nX\n\nC\n\nparts\n\nbase\n\nA\n\nS");
    assert!(!prompt.contains('R'));
}

#[test]
fn system_string_is_byte_stable_across_changing_reminder_sets() {
    // Same stable system fragments on both turns; the only difference is
    // the live reminder set. Turn N has no reminders; turn N+1 fires a
    // token-pressure reminder. The assembled `system` string must be
    // byte-identical so the non-Anthropic prefix cache stays warm.
    let options = BTreeMap::from([
        ("system_prompt_parts".to_string(), s("parts")),
        ("system_appendix".to_string(), s("appendix")),
    ]);

    let turn_n =
        compose_system_prompt_with_reminders(Some("base".to_string()), Some(&options), &[])
            .expect("system prompt")
            .expect("non-empty prompt");

    let pressure = "<system-reminder>\nContext is 82% full; wrap up.\n</system-reminder>";
    let turn_n_plus_1 = compose_system_prompt_with_reminders(
        Some("base".to_string()),
        Some(&options),
        &[RenderedReminder::SystemText(pressure.to_string())],
    )
    .expect("system prompt")
    .expect("non-empty prompt");

    assert_eq!(
        turn_n, turn_n_plus_1,
        "system string must not change when the reminder set changes"
    );
    assert!(!turn_n.contains("system-reminder"));

    // The reminder is present on turn N+1 — as the trailing user message,
    // not in the system string.
    let base_messages = || vec![serde_json::json!({"role": "user", "content": "hello"})];
    let msgs_n = apply_rendered_reminder_messages(base_messages(), &[]);
    let msgs_n_plus_1 = apply_rendered_reminder_messages(
        base_messages(),
        &[RenderedReminder::SystemText(pressure.to_string())],
    );
    // Turn N: no reminder anywhere in the message array.
    assert!(!serde_json::to_string(&msgs_n)
        .unwrap()
        .contains("system-reminder"));
    // Turn N+1: reminder folded into the trailing user turn (merged with
    // the existing user message rather than appended as a second user msg).
    assert_eq!(msgs_n_plus_1.len(), 1);
    let last = msgs_n_plus_1.last().expect("trailing message");
    assert_eq!(last["role"], "user");
    assert_eq!(last["content"], format!("hello\n\n{pressure}"));
}

#[test]
fn system_text_reminder_appends_new_user_message_after_assistant_tail() {
    // When the conversation tail is an assistant turn (e.g. a tool_call),
    // the reminder must be appended as a fresh trailing user message —
    // never merged into the assistant turn and never inserted before it.
    let messages = vec![
        serde_json::json!({"role": "user", "content": "do it"}),
        serde_json::json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{"id": "c1", "type": "function"}],
        }),
        serde_json::json!({"role": "tool", "tool_call_id": "c1", "content": "result"}),
        serde_json::json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{"id": "c2", "type": "function"}],
        }),
    ];
    let out = apply_rendered_reminder_messages(
        messages,
        &[RenderedReminder::SystemText(
            "<system-reminder>\nR\n</system-reminder>".to_string(),
        )],
    );
    assert_eq!(out.len(), 5);
    // The original assistant tool_call/tool_result ordering is preserved.
    assert_eq!(out[1]["role"], "assistant");
    assert_eq!(out[2]["role"], "tool");
    assert_eq!(out[3]["role"], "assistant");
    // The reminder is a brand-new trailing user message.
    assert_eq!(out[4]["role"], "user");
    assert_eq!(
        out[4]["content"],
        "<system-reminder>\nR\n</system-reminder>"
    );
}

#[test]
fn multiple_system_text_reminders_coalesce_into_one_trailing_message() {
    let out = apply_rendered_reminder_messages(
        vec![serde_json::json!({"role": "assistant", "content": "ok"})],
        &[
            RenderedReminder::SystemText("<system-reminder>\nA\n</system-reminder>".to_string()),
            RenderedReminder::SystemText("<system-reminder>\nB\n</system-reminder>".to_string()),
        ],
    );
    assert_eq!(out.len(), 2);
    assert_eq!(out[1]["role"], "user");
    assert_eq!(
        out[1]["content"],
        "<system-reminder>\nA\n</system-reminder>\n\n<system-reminder>\nB\n</system-reminder>"
    );
}

#[test]
fn dict_part_position_override_moves_to_after() {
    let options = BTreeMap::from([(
        "system_prompt_parts".to_string(),
        dict(&[("content", s("moved")), ("position", s("after"))]),
    )]);
    let prompt = compose_system_prompt(Some("base".to_string()), Some(&options))
        .expect("system prompt")
        .expect("non-empty prompt");
    assert_eq!(prompt, "base\n\nmoved");
}

#[test]
fn dict_part_with_title_renders_heading() {
    let options = BTreeMap::from([(
        "system_prompt_parts".to_string(),
        dict(&[("content", s("body")), ("title", s("Title"))]),
    )]);
    let prompt = compose_system_prompt(None, Some(&options))
        .expect("system prompt")
        .expect("non-empty prompt");
    assert_eq!(prompt, "## Title\nbody");
}

#[test]
fn list_parts_expand_in_declaration_order() {
    let options = BTreeMap::from([(
        "system_prompt_parts".to_string(),
        list(vec![s("one"), s("two")]),
    )]);
    let prompt = compose_system_prompt(None, Some(&options))
        .expect("system prompt")
        .expect("non-empty prompt");
    assert_eq!(prompt, "one\n\ntwo");
}

#[test]
fn nil_system_arg_falls_back_to_opts_system() {
    let options = BTreeMap::from([("system".to_string(), s("fromopts"))]);
    let prompt = compose_system_prompt(None, Some(&options))
        .expect("system prompt")
        .expect("non-empty prompt");
    assert_eq!(prompt, "fromopts");
}

#[test]
fn tool_guidance_is_injected_only_when_the_tool_is_present() {
    // Tool carrying `guidance` → instruction auto-included after primary.
    let with_guidance = BTreeMap::from([(
        "tools".to_string(),
        list(vec![dict(&[
            ("name", s("todo")),
            ("description", s("Track tasks")),
            (
                "guidance",
                s("Always update the TODO tracker when working from a plan."),
            ),
        ])]),
    )]);
    let prompt = compose_system_prompt(Some("base".to_string()), Some(&with_guidance))
        .expect("system prompt")
        .expect("non-empty prompt");
    assert_eq!(
        prompt,
        "base\n\nAlways update the TODO tracker when working from a plan."
    );

    // Same tool without `guidance`, or a different tool set → no fragment.
    let no_guidance = BTreeMap::from([(
        "tools".to_string(),
        list(vec![dict(&[
            ("name", s("read")),
            ("description", s("Read files")),
        ])]),
    )]);
    let prompt = compose_system_prompt(Some("base".to_string()), Some(&no_guidance))
        .expect("system prompt")
        .expect("non-empty prompt");
    assert_eq!(prompt, "base");
}

#[test]
fn assemble_records_provenance_for_every_fragment() {
    let options = BTreeMap::from([
        ("system_prompt_parts".to_string(), s("parts")),
        (
            "tools".to_string(),
            list(vec![dict(&[
                ("name", s("todo")),
                ("description", s("Track tasks")),
                ("guidance", s("Update the tracker.")),
            ])]),
        ),
    ]);
    let assembled =
        assemble_system_prompt(Some("base".to_string()), Some(&options), &[]).expect("assembled");
    // host:system_prompt_parts, primary, tool:todo.guidance — all included.
    let ids: Vec<&str> = assembled.provenance.iter().map(|t| t.id.as_str()).collect();
    assert!(ids.contains(&"host:system_prompt_parts"));
    assert!(ids.contains(&"primary"));
    assert!(ids.contains(&"tool:todo.guidance"));
    let todo = assembled
        .provenance
        .iter()
        .find(|t| t.id == "tool:todo.guidance")
        .expect("todo guidance trace");
    assert!(todo.included);
    assert!(todo.reason.contains("tool(s) present: todo"));
}

fn fragment(id: &str, body: &str) -> VmValue {
    dict(&[("id", s(id)), ("source", s("primary")), ("body", s(body))])
}

#[test]
fn system_fragments_expand_in_place_of_the_single_primary() {
    // The decomposed channel yields the same bytes as the equivalent
    // joined-string primary, while keeping each part individually traced.
    let decomposed = BTreeMap::from([
        ("system_prefix".to_string(), s("X")),
        (
            "_system_fragments".to_string(),
            list(vec![
                fragment("primary:system", "base"),
                fragment("primary:active_skills", "## Active skills"),
                fragment("primary:loop_contract", "Keep going until done."),
            ]),
        ),
        ("system_appendix".to_string(), s("A")),
    ]);
    let joined = "base\n\n## Active skills\n\nKeep going until done.";
    let baseline = BTreeMap::from([
        ("system_prefix".to_string(), s("X")),
        ("system_appendix".to_string(), s("A")),
    ]);

    let from_fragments = compose_system_prompt(None, Some(&decomposed))
        .expect("system prompt")
        .expect("non-empty prompt");
    let from_string = compose_system_prompt(Some(joined.to_string()), Some(&baseline))
        .expect("system prompt")
        .expect("non-empty prompt");
    assert_eq!(from_fragments, from_string);
    assert_eq!(
        from_fragments,
        "X\n\nbase\n\n## Active skills\n\nKeep going until done.\n\nA"
    );

    // Each part is its own provenance entry; there is no opaque `primary`.
    let assembled = assemble_system_prompt(None, Some(&decomposed), &[]).expect("assembled");
    let ids: Vec<&str> = assembled.provenance.iter().map(|t| t.id.as_str()).collect();
    assert!(ids.contains(&"primary:system"));
    assert!(ids.contains(&"primary:active_skills"));
    assert!(ids.contains(&"primary:loop_contract"));
    assert!(!ids.contains(&"primary"));
}

#[test]
fn system_fragments_supersede_the_system_arg() {
    // When the channel is present, the `system` arg / `opts.system` no
    // longer contributes a primary fragment — the channel owns that block.
    let options = BTreeMap::from([
        ("system".to_string(), s("ignored opts.system")),
        (
            "_system_fragments".to_string(),
            list(vec![fragment("primary:system", "decomposed")]),
        ),
    ]);
    let prompt = compose_system_prompt(Some("ignored system arg".to_string()), Some(&options))
        .expect("system prompt")
        .expect("non-empty prompt");
    assert_eq!(prompt, "decomposed");
}

#[test]
fn empty_system_fragments_yield_no_primary() {
    // An empty list still claims the primary block: the agent computed zero
    // non-empty parts, so the `system` arg must not leak back in.
    let options = BTreeMap::from([("_system_fragments".to_string(), list(vec![]))]);
    let prompt = compose_system_prompt(Some("should not appear".to_string()), Some(&options))
        .expect("system prompt");
    assert_eq!(prompt, None);
}

#[test]
fn system_fragments_honor_per_part_tool_gating() {
    let options = BTreeMap::from([(
        "_system_fragments".to_string(),
        list(vec![dict(&[
            ("id", s("primary:todo_nudge")),
            ("body", s("Keep the TODO list current.")),
            ("requires_tools", list(vec![s("todo")])),
        ])]),
    )]);
    // Tool absent → gated out, recorded with a reason.
    let assembled = assemble_system_prompt(None, Some(&options), &[]).expect("assembled");
    assert_eq!(assembled.system, None);
    let trace = assembled
        .provenance
        .iter()
        .find(|t| t.id == "primary:todo_nudge")
        .expect("nudge trace");
    assert!(!trace.included);
    assert!(trace.reason.contains("requires tool `todo`"));
}

#[test]
fn system_fragments_can_target_the_tail_bucket() {
    let options = BTreeMap::from([
        (
            "_system_fragments".to_string(),
            list(vec![
                fragment("primary:system", "base"),
                dict(&[
                    ("id", s("primary:scratchpad")),
                    ("source", s("primary")),
                    ("body", s("scratchpad tail")),
                    ("bucket", s("after")),
                ]),
            ]),
        ),
        ("system_suffix".to_string(), s("host suffix")),
    ]);

    let prompt = compose_system_prompt(None, Some(&options))
        .expect("system prompt")
        .expect("non-empty prompt");
    assert_eq!(prompt, "base\n\nhost suffix\n\nscratchpad tail");

    let assembled = assemble_system_prompt(None, Some(&options), &[]).expect("assembled");
    let trace = assembled
        .provenance
        .iter()
        .find(|t| t.id == "primary:scratchpad")
        .expect("scratchpad trace");
    assert_eq!(trace.bucket, "after");
}

#[test]
fn system_fragments_reject_unknown_bucket() {
    let options = BTreeMap::from([(
        "_system_fragments".to_string(),
        list(vec![dict(&[
            ("id", s("primary:bad")),
            ("body", s("bad")),
            ("bucket", s("middle")),
        ])]),
    )]);
    let error = assemble_system_prompt(None, Some(&options), &[]).unwrap_err();
    assert!(
        error.to_string().contains("bucket must be"),
        "unexpected error: {error}"
    );
}

#[test]
fn context_profile_fragments_join_prompt_explain_provenance() {
    let options = BTreeMap::from([(
        "context_profile".to_string(),
        dict(&[
            ("caps", list(vec![s("remote.github")])),
            (
                "prompt_fragments",
                list(vec![dict(&[
                    ("id", s("profile:github")),
                    ("source", s("profile:github")),
                    ("body", s("Use GitHub-aware workflows.")),
                    ("requires_caps", list(vec![s("remote.github")])),
                ])]),
            ),
        ]),
    )]);

    let assembled = assemble_system_prompt(None, Some(&options), &[]).expect("assembled");
    assert_eq!(
        assembled.system.as_deref(),
        Some("Use GitHub-aware workflows.")
    );
    let trace = assembled
        .provenance
        .iter()
        .find(|trace| trace.id == "profile:github")
        .expect("profile trace");
    assert!(trace.included);
    assert!(trace.reason.contains("capabilit"));
}
