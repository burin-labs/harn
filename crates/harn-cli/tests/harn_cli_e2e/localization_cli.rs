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
