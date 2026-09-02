use super::*;
use super::{reminders::*, system_prompt::*};
use crate::llm::helpers::{DirectiveAuthority, ReminderRoleHint};

fn reminder(
    role_hint: ReminderRoleHint,
    authority: DirectiveAuthority,
    body: &str,
) -> SystemReminder {
    SystemReminder {
        id: "reminder-1".to_string(),
        tags: vec!["test".to_string()],
        dedupe_key: None,
        ttl_turns: None,
        preserve_on_compact: false,
        propagate: crate::llm::helpers::ReminderPropagate::Session,
        role_hint,
        authority,
        source: crate::llm::helpers::ReminderSource::InPipeline,
        body: body.to_string(),
        fired_at_turn: 0,
        originating_agent_id: None,
    }
}

#[test]
fn every_route_and_legacy_role_hint_use_the_same_directive_shape() {
    crate::llm::capabilities::clear_user_overrides();
    for (provider, model) in [
        ("mock", "claude-sonnet-4-7"),
        ("mock", "o3"),
        ("gemini", "gemini-2.5-flash"),
    ] {
        let caps = crate::llm::capabilities::lookup(provider, model);
        for role_hint in [
            ReminderRoleHint::System,
            ReminderRoleHint::Developer,
            ReminderRoleHint::UserBlock,
            ReminderRoleHint::EphemeralCache,
        ] {
            let rendered = render_pending_reminders(
                &caps,
                &[reminder(
                    role_hint,
                    DirectiveAuthority::Corrective,
                    "remember <this>",
                )],
            );
            assert_eq!(
                rendered,
                vec![RenderedReminder::tracked(
                    "reminder-1",
                    "<directive authority=\"corrective\">\nremember &lt;this&gt;\n</directive>",
                )]
            );
        }
    }
}

#[test]
fn finite_lifetime_is_part_of_the_model_facing_directive() {
    let mut finite = reminder(
        ReminderRoleHint::System,
        DirectiveAuthority::Corrective,
        "verify once",
    );
    finite.ttl_turns = Some(1);
    let rendered = render_pending_reminders(
        &crate::llm::capabilities::Capabilities::default(),
        &[finite],
    );
    assert_eq!(
        rendered,
        vec![RenderedReminder::tracked(
            "reminder-1",
            "<directive authority=\"corrective\" ttl_turns=\"1\">\nverify once\n</directive>",
        )]
    );
}

#[test]
fn directive_instance_receipts_are_stripped_before_provider_dispatch() {
    let tracked = RenderedReminder::tracked(
        "reminder-1",
        "<directive authority=\"corrective\" ttl_turns=\"1\">\nverify once\n</directive>",
    );
    let mut messages = apply_rendered_reminder_messages(Vec::new(), &[tracked]);
    assert_eq!(
        messages[0][DIRECTIVE_IDS_KEY],
        serde_json::json!(["reminder-1"])
    );
    strip_directive_commit_metadata(&mut messages);
    assert!(messages[0].get(DIRECTIVE_IDS_KEY).is_none());
    assert!(messages[0]["content"]
        .as_str()
        .is_some_and(|content| content.contains("ttl_turns=\"1\"")));
}

#[test]
fn directive_envelope_uses_the_instruction_asset_verbatim() {
    let source = harn_stdlib::get_stdlib_prompt_asset(DIRECTIVE_ENVELOPE_INSTRUCTIONS_ASSET)
        .expect("directive envelope instruction prompt asset");
    let directive = RenderedReminder::untracked(
        "<directive authority=\"corrective\" ttl_turns=\"1\">\nverify once\n</directive>",
    );
    let expected = format!(
        "<context-directives>\n{}\n{}\n</context-directives>",
        source.trim_end(),
        directive.text()
    );
    assert_eq!(directive_envelope(&[directive]), Some(expected));
}

#[test]
fn system_text_reminders_are_excluded_from_system_string() {
    let options = crate::value::DictMap::from_iter([(
        "system".to_string(),
        list(vec![
            s("parts"),
            dict(&[("content", s("appendix")), ("position", s("after"))]),
        ]),
    )]);
    // A directive must not enter the cache-sensitive system string.
    let prompt = compose_system_prompt_with_reminders(
        Some("base".to_string()),
        Some(&options),
        &[RenderedReminder::untracked("reminder")],
    )
    .expect("system prompt")
    .expect("non-empty prompt");
    assert_eq!(prompt, "parts\n\nbase\n\nappendix");

    // The directive instead lands in the one trailing user envelope.
    let messages = apply_rendered_reminder_messages(
        vec![serde_json::json!({"role": "assistant", "content": "ok"})],
        &[RenderedReminder::untracked("reminder")],
    );
    let last = messages.last().expect("trailing message");
    assert_eq!(last["role"], "user");
    assert_eq!(
        last["content"],
        format!(
            "<context-directives>\n{}\nreminder\n</context-directives>",
            directive_envelope_instructions()
        )
    );
}

fn s(text: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(text))
}

fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
    VmValue::dict(
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect::<crate::value::DictMap>(),
    )
}

fn list(items: Vec<VmValue>) -> VmValue {
    VmValue::List(std::sync::Arc::new(items))
}

#[test]
fn full_host_option_ordering_is_faithful() {
    // The single `system` LIST carries all surrounding fragments: the four
    // "before" fragments in list order, then (after the primary) the two
    // "after" fragments in list order.
    let options = crate::value::DictMap::from_iter([(
        "system".to_string(),
        list(vec![
            s("P"),
            s("X"),
            s("C"),
            s("parts"),
            dict(&[("content", s("A")), ("position", s("after"))]),
            dict(&[("content", s("S")), ("position", s("after"))]),
        ]),
    )]);
    let prompt = compose_system_prompt_with_reminders(
        Some("base".to_string()),
        Some(&options),
        &[RenderedReminder::untracked("R")],
    )
    .expect("system prompt")
    .expect("non-empty prompt");
    // Before bucket in list order (P, X, C, parts, primary), then After
    // bucket (A, S). The `SystemText` reminder ("R") is no longer folded into
    // the system string — it is appended as a trailing message instead.
    assert_eq!(prompt, "P\n\nX\n\nC\n\nparts\n\nbase\n\nA\n\nS");
    assert!(!prompt.contains('R'));
}

#[test]
fn system_string_is_byte_stable_across_changing_reminder_sets() {
    // Same stable system fragments on both turns; the only difference is
    // the live reminder set. Turn N has no reminders; turn N+1 fires a
    // token-pressure reminder. The assembled `system` string must be
    // byte-identical so the non-Anthropic prefix cache stays warm.
    let options = crate::value::DictMap::from_iter([(
        "system".to_string(),
        list(vec![
            s("parts"),
            dict(&[("content", s("appendix")), ("position", s("after"))]),
        ]),
    )]);

    let turn_n =
        compose_system_prompt_with_reminders(Some("base".to_string()), Some(&options), &[])
            .expect("system prompt")
            .expect("non-empty prompt");

    let pressure =
        "<directive authority=\"advisory\">\nContext is 82% full; wrap up.\n</directive>";
    let turn_n_plus_1 = compose_system_prompt_with_reminders(
        Some("base".to_string()),
        Some(&options),
        &[RenderedReminder::untracked(pressure)],
    )
    .expect("system prompt")
    .expect("non-empty prompt");

    assert_eq!(
        turn_n, turn_n_plus_1,
        "system string must not change when the reminder set changes"
    );
    assert!(!turn_n.contains("context-directives"));

    // The reminder is present on turn N+1 — as its own trailing user message,
    // not in the system string and not merged into the turn already there.
    let base_messages = || vec![serde_json::json!({"role": "user", "content": "hello"})];
    let msgs_n = apply_rendered_reminder_messages(base_messages(), &[]);
    let msgs_n_plus_1 =
        apply_rendered_reminder_messages(base_messages(), &[RenderedReminder::untracked(pressure)]);
    // Turn N: no reminder anywhere in the message array.
    assert!(!serde_json::to_string(&msgs_n)
        .unwrap()
        .contains("context-directives"));
    // Turn N+1 extends turn N rather than editing it: message 0 is untouched,
    // and the envelope arrives as a new message after it. That is what keeps
    // the provider's prompt prefix reusable across the two requests.
    assert_eq!(msgs_n_plus_1.len(), 2);
    assert_eq!(msgs_n_plus_1[0], msgs_n[0]);
    assert_eq!(msgs_n_plus_1[0]["content"], "hello");
    assert_eq!(msgs_n_plus_1[1]["role"], "user");
    assert_eq!(
        msgs_n_plus_1[1]["content"],
        format!(
            "<context-directives>\n{}\n{pressure}\n</context-directives>",
            directive_envelope_instructions()
        )
    );
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
        &[RenderedReminder::untracked(
            "<directive authority=\"contract\">\nR\n</directive>",
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
        [
            "<context-directives>",
            directive_envelope_instructions(),
            "<directive authority=\"contract\">",
            "R",
            "</directive>",
            "</context-directives>",
        ]
        .join("\n")
    );
}

#[test]
fn multiple_system_text_reminders_coalesce_into_one_trailing_message() {
    let out = apply_rendered_reminder_messages(
        vec![serde_json::json!({"role": "assistant", "content": "ok"})],
        &[
            RenderedReminder::untracked("<directive authority=\"contract\">\nA\n</directive>"),
            RenderedReminder::untracked("<directive authority=\"corrective\">\nB\n</directive>"),
        ],
    );
    assert_eq!(out.len(), 2);
    assert_eq!(out[1]["role"], "user");
    assert_eq!(
        out[1]["content"],
        [
            "<context-directives>",
            directive_envelope_instructions(),
            "<directive authority=\"contract\">",
            "A",
            "</directive>",
            "<directive authority=\"corrective\">",
            "B",
            "</directive>",
            "</context-directives>",
        ]
        .join("\n")
    );
}

#[test]
fn fragment_position_override_moves_to_after() {
    let options = crate::value::DictMap::from_iter([(
        "system".to_string(),
        list(vec![dict(&[
            ("content", s("moved")),
            ("position", s("after")),
        ])]),
    )]);
    let prompt = compose_system_prompt(Some("base".to_string()), Some(&options))
        .expect("system prompt")
        .expect("non-empty prompt");
    assert_eq!(prompt, "base\n\nmoved");
}

#[test]
fn fragment_with_title_renders_heading() {
    let options = crate::value::DictMap::from_iter([(
        "system".to_string(),
        list(vec![dict(&[("content", s("body")), ("title", s("Title"))])]),
    )]);
    let prompt = compose_system_prompt(None, Some(&options))
        .expect("system prompt")
        .expect("non-empty prompt");
    assert_eq!(prompt, "## Title\nbody");
}

#[test]
fn string_fragments_expand_in_list_order() {
    let options =
        crate::value::DictMap::from_iter([("system".to_string(), list(vec![s("one"), s("two")]))]);
    let prompt = compose_system_prompt(None, Some(&options))
        .expect("system prompt")
        .expect("non-empty prompt");
    assert_eq!(prompt, "one\n\ntwo");
}

#[test]
fn list_form_partitions_before_and_after_around_primary() {
    // A single `system` list mixing "before" and "after" fragments assembles
    // as: before fragments in list order, primary, then after fragments in
    // list order.
    let options = crate::value::DictMap::from_iter([(
        "system".to_string(),
        list(vec![
            s("head"),
            dict(&[("content", s("tail")), ("position", s("after"))]),
            dict(&[("content", s("head2")), ("position", s("before"))]),
        ]),
    )]);
    let prompt = compose_system_prompt(Some("base".to_string()), Some(&options))
        .expect("system prompt")
        .expect("non-empty prompt");
    assert_eq!(prompt, "head\n\nhead2\n\nbase\n\ntail");
}

#[test]
fn fragment_dict_alias_key_is_rejected() {
    // `text` was a legacy alias for `content`; it is now a hard error naming
    // the canonical fragment shape.
    let options = crate::value::DictMap::from_iter([(
        "system".to_string(),
        list(vec![dict(&[("text", s("body"))])]),
    )]);
    let error = compose_system_prompt(None, Some(&options)).unwrap_err();
    let msg = match error {
        VmError::Thrown(VmValue::String(s)) => s.to_string(),
        other => format!("{other:?}"),
    };
    assert!(msg.contains("`text` was removed"), "{msg}");
    assert!(msg.contains("use `content`"), "{msg}");
    assert!(msg.contains("content: string"), "{msg}");
}

#[test]
fn fragment_dict_unknown_key_is_rejected() {
    let options = crate::value::DictMap::from_iter([(
        "system".to_string(),
        list(vec![dict(&[("content", s("body")), ("weight", s("3"))])]),
    )]);
    let error = compose_system_prompt(None, Some(&options)).unwrap_err();
    let msg = match error {
        VmError::Thrown(VmValue::String(s)) => s.to_string(),
        other => format!("{other:?}"),
    };
    assert!(msg.contains("unknown fragment key `weight`"), "{msg}");
    assert!(msg.contains("position?: \"before\"|\"after\""), "{msg}");
}

#[test]
fn nil_system_arg_falls_back_to_opts_system() {
    let options = crate::value::DictMap::from_iter([("system".to_string(), s("fromopts"))]);
    let prompt = compose_system_prompt(None, Some(&options))
        .expect("system prompt")
        .expect("non-empty prompt");
    assert_eq!(prompt, "fromopts");
}

#[test]
fn tool_guidance_is_injected_only_when_the_tool_is_present() {
    // Tool carrying `guidance` → instruction auto-included after primary.
    let with_guidance = crate::value::DictMap::from_iter([(
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
    let no_guidance = crate::value::DictMap::from_iter([(
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
    let options = crate::value::DictMap::from_iter([
        ("system".to_string(), list(vec![s("parts")])),
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
    // system[0], primary, tool:todo.guidance — all included.
    let ids: Vec<&str> = assembled
        .manifest()
        .segments()
        .iter()
        .map(|t| t.id.as_str())
        .collect();
    assert!(ids.contains(&"system[0]"));
    assert!(ids.contains(&"primary"));
    assert!(ids.contains(&"tool:todo.guidance"));
    let todo = assembled
        .manifest()
        .segments()
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
    let host_list = || {
        list(vec![
            dict(&[("content", s("X"))]),
            dict(&[("content", s("A")), ("position", s("after"))]),
        ])
    };
    let decomposed = crate::value::DictMap::from_iter([
        ("system".to_string(), host_list()),
        (
            "_system_fragments".to_string(),
            list(vec![
                fragment("primary:system", "base"),
                fragment("primary:active_skills", "## Active skills"),
                fragment("primary:loop_contract", "Keep going until done."),
            ]),
        ),
    ]);
    let joined = "base\n\n## Active skills\n\nKeep going until done.";
    let baseline = crate::value::DictMap::from_iter([("system".to_string(), host_list())]);

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
    let ids: Vec<&str> = assembled
        .manifest()
        .segments()
        .iter()
        .map(|t| t.id.as_str())
        .collect();
    assert!(ids.contains(&"primary:system"));
    assert!(ids.contains(&"primary:active_skills"));
    assert!(ids.contains(&"primary:loop_contract"));
    assert!(!ids.contains(&"primary"));
}

#[test]
fn system_fragments_supersede_the_system_arg() {
    // When the channel is present, the `system` arg / `opts.system` no
    // longer contributes a primary fragment — the channel owns that block.
    let options = crate::value::DictMap::from_iter([
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
    let options =
        crate::value::DictMap::from_iter([("_system_fragments".to_string(), list(vec![]))]);
    let prompt = compose_system_prompt(Some("should not appear".to_string()), Some(&options))
        .expect("system prompt");
    assert_eq!(prompt, None);
}

#[test]
fn system_fragments_honor_per_part_tool_gating() {
    let options = crate::value::DictMap::from_iter([(
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
        .manifest()
        .segments()
        .iter()
        .find(|t| t.id == "primary:todo_nudge")
        .expect("nudge trace");
    assert!(!trace.included);
    assert!(trace.reason.contains("requires tool `todo`"));
}

#[test]
fn system_fragments_can_target_the_tail_bucket() {
    let options = crate::value::DictMap::from_iter([
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
        (
            "system".to_string(),
            list(vec![dict(&[
                ("content", s("host suffix")),
                ("position", s("after")),
            ])]),
        ),
    ]);

    let prompt = compose_system_prompt(None, Some(&options))
        .expect("system prompt")
        .expect("non-empty prompt");
    assert_eq!(prompt, "base\n\nhost suffix\n\nscratchpad tail");

    let assembled = assemble_system_prompt(None, Some(&options), &[]).expect("assembled");
    let trace = assembled
        .manifest()
        .segments()
        .iter()
        .find(|t| t.id == "primary:scratchpad")
        .expect("scratchpad trace");
    assert_eq!(trace.bucket, "after");
}

#[test]
fn system_fragments_reject_unknown_bucket() {
    let options = crate::value::DictMap::from_iter([(
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
fn system_replacement_rejects_noncanonical_shapes_at_the_assembly_boundary() {
    let cases = [
        (
            dict(&[("mode", s("append")), ("content", s("prompt"))]),
            "system.mode: expected \"replace\"",
        ),
        (
            dict(&[("mode", s("replace")), ("text", s("prompt"))]),
            "unknown replacement key `text`",
        ),
        (
            dict(&[("mode", s("replace")), ("content", VmValue::Bool(true))]),
            "system.content: expected a string",
        ),
    ];

    for (system, expected) in cases {
        let options = crate::value::DictMap::from_iter([("system".to_string(), system)]);
        let error = assemble_system_prompt(None, Some(&options), &[]).unwrap_err();
        assert!(
            error.to_string().contains(expected),
            "expected {expected:?}, got {error}"
        );
    }
}

#[test]
fn context_profile_fragments_join_prompt_explain_provenance() {
    let options = crate::value::DictMap::from_iter([(
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
        .manifest()
        .segments()
        .iter()
        .find(|trace| trace.id == "profile:github")
        .expect("profile trace");
    assert!(trace.included);
    assert!(trace.reason.contains("capabilit"));
}

#[test]
fn context_manifest_carries_the_current_delegated_actor_chain() {
    crate::reset_thread_local_state();
    let chain = crate::ActorChain::new("user:test-owner").pushed("agent:reviewer");
    let session_id = crate::agent_sessions::open_or_create_with_actor_chain_for_test(
        Some("context-manifest-actor-chain".to_string()),
        Some(chain.clone()),
    );
    let _session = crate::agent_sessions::enter_current_session(session_id);

    let assembled =
        assemble_system_prompt(Some("review carefully".to_string()), None, &[]).expect("assembled");
    assert_eq!(
        assembled.manifest().actor_chain(),
        Some(&chain.to_json_value())
    );
}
