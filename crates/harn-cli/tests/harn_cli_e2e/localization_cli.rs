//! Locale selection must reach the shipped CLI, including nested help.

use std::process::Output;

use crate::test_util::process::harn_e2e_command;

const LOCALES: [(&str, &str); 9] = [
    ("en_US.UTF-8", "Usage"),
    ("es_ES.UTF-8", "Uso"),
    ("fr_CA.UTF-8", "Utilisation"),
    ("de_DE.UTF-8", "Verwendung"),
    ("pt_BR.UTF-8", "Uso"),
    ("it_IT.UTF-8", "Uso"),
    ("ja_JP.UTF-8", "使い方"),
    ("ko_KR.UTF-8", "사용법"),
    ("zh_CN.UTF-8", "用法"),
];

fn run(locale: &str, arguments: &[&str]) -> Output {
    harn_e2e_command()
        .env("LC_ALL", locale)
        .env_remove("LC_MESSAGES")
        .env("LANG", "en_US.UTF-8")
        .args(arguments)
        .output()
        .expect("run localized CLI")
}

#[test]
fn browser_independent_locales_reach_root_and_nested_cli_help() {
    for (locale, heading) in LOCALES {
        for arguments in [&["--help"][..], &["run", "--help"][..]] {
            let output = run(locale, arguments);
            assert!(output.status.success(), "{locale}: {:?}", output.status);
            let help = String::from_utf8(output.stdout).expect("UTF-8 help");
            assert!(
                help.contains(&format!("{heading}: harn")),
                "{locale}: {help}"
            );
            assert!(help.contains("--help"), "{locale}: stable help flag");
            assert!(help.contains("run"), "{locale}: stable command name");
            if locale != "en_US.UTF-8" {
                assert!(!help.contains("Usage: harn"), "{locale}: English fallback");
            }
        }
    }
}

#[test]
fn posix_locale_precedence_and_unsupported_scripts_reach_the_cli() {
    for (all, messages, lang, heading) in [
        (Some("it_IT.UTF-8"), Some("fr"), "ja", "Uso"),
        (None, Some("fr"), "ja", "Utilisation"),
        (Some(""), Some("de"), "ja", "Verwendung"),
        (None, None, "ja_JP.UTF-8", "使い方"),
        (Some("zh_Hant_TW.UTF-8"), Some("fr"), "ja", "Usage"),
        (Some("C.UTF-8"), Some("fr"), "ja", "Usage"),
    ] {
        let mut command = harn_e2e_command();
        command
            .env_remove("LC_ALL")
            .env_remove("LC_MESSAGES")
            .env("LANG", lang);
        if let Some(locale) = all {
            command.env("LC_ALL", locale);
        }
        if let Some(locale) = messages {
            command.env("LC_MESSAGES", locale);
        }
        let output = command
            .arg("--help")
            .output()
            .expect("locale precedence help");
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout).expect("UTF-8 help");
        assert!(
            help.contains(&format!("{heading}: harn")),
            "{all:?}, {messages:?}, {lang}: {help}"
        );
    }
}

#[test]
fn localizing_help_keeps_structured_arguments_and_diagnostics_canonical() {
    let baseline = run("C", &["--argument-schema"]);
    assert!(baseline.status.success());
    let schema: serde_json::Value =
        serde_json::from_slice(&baseline.stdout).expect("argument schema");
    assert_eq!(schema["data"]["name"], "harn");
    assert!(
        schema["data"]["subcommands"]
            .as_array()
            .expect("populated parser tree")
            .len()
            > 30
    );
    for (locale, _) in LOCALES {
        let output = run(locale, &["--argument-schema"]);
        assert!(output.status.success(), "{locale}");
        assert_eq!(
            output.stdout, baseline.stdout,
            "{locale}: argument contract changed"
        );
        let invalid = run(locale, &["--this-flag-does-not-exist"]);
        assert!(!invalid.status.success(), "{locale}: invalid flag accepted");
        assert!(
            String::from_utf8_lossy(&invalid.stderr).contains("unexpected argument"),
            "{locale}: diagnostic changed"
        );
    }
}

#[test]
fn command_results_use_selected_language_while_json_and_csv_stay_canonical() {
    let directory = tempfile::tempdir().expect("isolated usage project");
    let project = directory.path().to_str().unwrap();
    let source = "No LLM usage recorded (0 provider_call_response events matched).";
    for (locale, id) in [
        ("C", "en"),
        ("es", "es"),
        ("fr", "fr"),
        ("de", "de"),
        ("pt_BR", "pt-BR"),
        ("it", "it"),
        ("ja", "ja"),
        ("ko", "ko"),
        ("zh_CN", "zh-Hans"),
    ] {
        let output = run(locale, &["usage", "--project", project]);
        assert!(output.status.success(), "{locale}: {output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        let expected = if id == "en" {
            source.to_owned()
        } else {
            let catalog: std::collections::BTreeMap<String, String> =
                serde_json::from_str(match id {
                    "es" => include_str!("../../src/localization/es.json"),
                    "fr" => include_str!("../../src/localization/fr.json"),
                    "de" => include_str!("../../src/localization/de.json"),
                    "pt-BR" => include_str!("../../src/localization/pt-BR.json"),
                    "it" => include_str!("../../src/localization/it.json"),
                    "ja" => include_str!("../../src/localization/ja.json"),
                    "ko" => include_str!("../../src/localization/ko.json"),
                    "zh-Hans" => include_str!("../../src/localization/zh-Hans.json"),
                    _ => unreachable!(),
                })
                .unwrap();
            catalog[source].clone()
        };
        assert!(text.contains(&expected), "{locale}: {text}");
        assert!(
            text.contains("provider_call_response"),
            "event ID must stay canonical"
        );
        let listing = run(locale, &["demo", "--list"]);
        assert!(listing.status.success(), "{locale}: {listing:?}");
        assert!(String::from_utf8_lossy(&listing.stdout).contains("merge-captain"));
    }
    for format in ["--json", "--csv"] {
        let english = run("C", &["usage", "--project", project, format]);
        let japanese = run("ja", &["usage", "--project", project, format]);
        assert!(english.status.success(), "{format}: {english:?}");
        assert!(japanese.status.success(), "{format}: {japanese:?}");
        assert!(!english.stdout.is_empty(), "must measure populated output");
        assert_eq!(english.stdout, japanese.stdout, "{format} output changed");
    }
}

#[test]
fn localized_orchestrator_records_preserve_field_names_and_raw_result_values() {
    use std::collections::{BTreeMap, BTreeSet};
    let expected: BTreeSet<_> = [
        "binding_id",
        "binding_version",
        "event_id",
        "status",
        "replay_of_event_id",
        "dlq_entry_id",
        "error",
        "result",
    ]
    .into_iter()
    .collect();
    for (locale, _) in LOCALES {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("harn.toml"),
            r#"
[package]
name = "localized-console-record"
[[triggers]]
id = "incoming-review-task"
kind = "a2a-push"
provider = "a2a-push"
match = { events = ["a2a.task.received"] }
handler = "worker://triage"
priority = "high"
"#,
        )
        .unwrap();
        let run_local = |arguments: &[&str]| {
            harn_e2e_command()
                .current_dir(project.path())
                .env("LC_ALL", locale)
                .args(arguments)
                .output()
                .expect("run local orchestrator")
        };
        let fire = run_local(&[
            "orchestrator",
            "fire",
            "incoming-review-task",
            "--config",
            "harn.toml",
            "--state-dir",
            "state",
        ]);
        assert!(fire.status.success(), "{locale}: {fire:?}");
        let text = String::from_utf8(fire.stdout).unwrap();
        let fields: BTreeMap<_, _> = text
            .lines()
            .filter_map(|line| line.strip_prefix("- ").and_then(|row| row.split_once('=')))
            .collect();
        assert_eq!(
            fields.keys().copied().collect::<BTreeSet<_>>(),
            expected,
            "{locale}: {text}"
        );
        assert_eq!(fields["binding_id"], "incoming-review-task");
        let result: serde_json::Value = serde_json::from_str(fields["result"])
            .expect("raw result braces must remain literal JSON");
        assert_eq!(result["queue"], "triage", "{locale}: {text}");
        let queue = run_local(&[
            "orchestrator",
            "queue",
            "--config",
            "harn.toml",
            "--state-dir",
            "state",
            "ls",
        ]);
        assert!(queue.status.success(), "{locale}: {queue:?}");
        let queue_text = String::from_utf8(queue.stdout).unwrap();
        assert!(
            queue_text.contains("triage"),
            "must reach the populated worker queue: {queue_text}"
        );
        let in_flight = queue_text
            .lines()
            .find_map(|line| line.strip_prefix("- dispatcher_in_flight="))
            .unwrap_or_else(|| panic!("{locale}: missing stable field: {queue_text}"));
        in_flight
            .parse::<u64>()
            .expect("field retains its numeric value");
        let literal = "literal{0}{not_a_field}%@100%";
        let matched = run_local(&["skill", "match", literal]);
        assert!(matched.status.success(), "{locale}: {matched:?}");
        assert!(
            String::from_utf8_lossy(&matched.stdout).contains(literal),
            "argument braces and percent sequences must remain literal: {matched:?}"
        );
    }
}
