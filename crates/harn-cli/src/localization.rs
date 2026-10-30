//! Human-readable CLI help is a projection of the owning Clap tree.
//! Command names, argument IDs, values, wire output, and diagnostics stay stable.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use clap::{Command, CommandFactory};
pub(crate) use harn_hostlib::localization::Locale;
use harn_hostlib::localization::{self as shared, process_locale};

use crate::cli::Cli;

include!(concat!(env!("OUT_DIR"), "/localization_formats.rs"));

#[cfg(test)]
#[path = "../build_support/localization_contract.rs"]
mod field_contract;

pub(crate) fn selected_locale() -> Locale {
    process_locale()
}

const CATALOGS: [(&str, &str); 8] = [
    ("es", include_str!("localization/es.json")),
    ("fr", include_str!("localization/fr.json")),
    ("de", include_str!("localization/de.json")),
    ("pt-BR", include_str!("localization/pt-BR.json")),
    ("it", include_str!("localization/it.json")),
    ("ja", include_str!("localization/ja.json")),
    ("ko", include_str!("localization/ko.json")),
    ("zh-Hans", include_str!("localization/zh-Hans.json")),
];
type Catalog = BTreeMap<String, String>;

#[derive(serde::Deserialize)]
struct ScriptCatalog {
    locale_env: String,
    messages: BTreeMap<String, usize>,
}

static SCRIPT_CATALOG: LazyLock<ScriptCatalog> = LazyLock::new(|| {
    serde_json::from_str(include_str!("localization_scripts.json"))
        .expect("checked CLI script message registry")
});

pub(crate) fn script_locale_env() -> &'static str {
    &SCRIPT_CATALOG.locale_env
}
pub(crate) const API_KEY_PROMPT: &str = "API key: ";
pub(crate) const OAUTH_SECRET_PROMPT: &str = "OAuth client secret: ";
pub(crate) const OAUTH_REDIRECT_PROMPT: &str = "OAuth redirect URI registered for this client: ";
pub(crate) const OAUTH_AUTHORIZATION_PROMPT: &str =
    "OAuth authorization URL (Enter to discover from resource): ";
pub(crate) const OAUTH_MISSING_REDIRECT: &str =
    "The old OAuth credential did not record its registered redirect URI.";
pub(crate) const OAUTH_MISSING_AUTHORIZATION: &str =
    "The old OAuth credential did not record its authorization URL.";
pub(crate) const OAUTH_COMPLETE_TITLE: &str = "Authorization Complete";
pub(crate) const OAUTH_FAILED_TITLE: &str = "Authorization Failed";
pub(crate) const OAUTH_CALLBACK_ERROR: &str = "Callback Error";
pub(crate) const OAUTH_CONNECTED: &str = "Connected";
pub(crate) const OAUTH_RETRY_NEEDED: &str = "Retry Needed";
pub(crate) const OAUTH_INVALID_REQUEST: &str = "Invalid Request";
pub(crate) const OAUTH_CLOSE_TAB: &str = "You can close this tab and return to Harn.";
pub(crate) const OAUTH_SUCCESS_MESSAGE: &str = "Authorization complete. You can close this window.";
pub(crate) const OAUTH_INVALID_PATH: &str = "Invalid callback path";
pub(crate) const OAUTH_MISSING_CODE: &str = "Missing authorization code";
pub(crate) const OAUTH_MISSING_STATE: &str = "Missing state";
pub(crate) const OAUTH_STATE_MISMATCH: &str = "State mismatch";
pub(crate) const OAUTH_FAILED_PREFIX: &str = "Authorization failed: ";
pub(crate) const OAUTH_DENIED: &str = "OAuth authorization was denied";
pub(crate) const OAUTH_CALLBACK_STATE_MISMATCH: &str = "OAuth callback state did not match";
pub(crate) const OAUTH_CALLBACK_TIMEOUT: &str = "OAuth callback timed out after 5 minutes";
pub(crate) const DOCTOR_PICK_MODEL: &str =
    "Run `harn models recommend` to pick a starter model for your machine.";
pub(crate) const DOCTOR_SCAFFOLD: &str =
    "Run `harn new my-agent --template agent` to scaffold a project.";
pub(crate) const DOCTOR_READY: &str =
    "You're ready. Try `harn try \"summarize this README\"` or `harn demo` for a bundled scenario.";
pub(crate) const DOCTOR_FIX_CHECKS: &str =
    "Address the failing checks above, then re-run `harn doctor`.";
const RUNTIME_MESSAGES: [&str; 26] = [
    DOCTOR_PICK_MODEL,
    DOCTOR_SCAFFOLD,
    DOCTOR_READY,
    DOCTOR_FIX_CHECKS,
    API_KEY_PROMPT,
    OAUTH_SECRET_PROMPT,
    OAUTH_REDIRECT_PROMPT,
    OAUTH_AUTHORIZATION_PROMPT,
    OAUTH_MISSING_REDIRECT,
    OAUTH_MISSING_AUTHORIZATION,
    OAUTH_COMPLETE_TITLE,
    OAUTH_FAILED_TITLE,
    OAUTH_CALLBACK_ERROR,
    OAUTH_CONNECTED,
    OAUTH_RETRY_NEEDED,
    OAUTH_INVALID_REQUEST,
    OAUTH_CLOSE_TAB,
    OAUTH_SUCCESS_MESSAGE,
    OAUTH_INVALID_PATH,
    OAUTH_MISSING_CODE,
    OAUTH_MISSING_STATE,
    OAUTH_STATE_MISMATCH,
    OAUTH_FAILED_PREFIX,
    OAUTH_DENIED,
    OAUTH_CALLBACK_STATE_MISMATCH,
    OAUTH_CALLBACK_TIMEOUT,
];
static TRANSLATIONS: LazyLock<BTreeMap<&str, Catalog>> = LazyLock::new(|| {
    CATALOGS
        .iter()
        .map(|(locale, json)| {
            (
                *locale,
                serde_json::from_str(json).expect("checked help catalog"),
            )
        })
        .collect()
});

pub(crate) fn command() -> Command {
    command_for(process_locale().id())
}

pub(crate) fn translate(source: &str) -> String {
    translate_for(process_locale(), source)
}

pub(crate) fn translate_for(locale: Locale, source: &str) -> String {
    let locale = locale.id();
    if locale == "en" {
        return source.to_owned();
    }
    TRANSLATIONS.get(locale).map_or_else(
        || source.to_owned(),
        |catalog| shared::translated_text(catalog, source),
    )
}

fn command_for(locale: &str) -> Command {
    let command = Cli::command();
    if locale == "en" {
        return command;
    }
    match TRANSLATIONS.get(locale) {
        Some(catalog) => shared::localize_command(command, catalog),
        None => command,
    }
}

/// Canonical English source for translators and the existing CLI drift guard.
pub(crate) fn source_catalog() -> Catalog {
    let mut catalog = shared::collect_help_messages(Cli::command());
    for source in SCRIPT_CATALOG.messages.keys() {
        catalog.insert(source.clone(), source.clone());
    }
    catalog.extend(
        serde_json::from_str::<Catalog>(include_str!("localization_formats.json"))
            .expect("checked command-result templates"),
    );
    for source in RUNTIME_MESSAGES {
        catalog.insert(source.to_owned(), source.to_owned());
    }
    catalog
}

pub(crate) fn dump_catalog(path: &str, check: bool) {
    let json =
        serde_json::to_string_pretty(&source_catalog()).expect("serializable help catalog") + "\n";
    crate::commands::generated_artifact::write_or_check(
        std::path::Path::new(path),
        &json,
        check,
        "the Clap help metadata",
        "gen-cli-surface",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_regions_and_scripts_follow_posix_names() {
        for (input, expected) in [
            ("fr_CA.UTF-8", Some("fr")),
            ("pt_PT", Some("pt-BR")),
            ("zh_CN.UTF-8", Some("zh-Hans")),
            ("zh_Hant_TW", None),
            ("C.UTF-8", Some("en")),
            ("unknown", None),
        ] {
            assert_eq!(
                shared::resolve_locale(input).map(shared::Locale::id),
                expected,
                "{input}"
            );
        }
    }

    #[test]
    fn every_help_catalog_covers_the_live_parser() {
        let source = source_catalog();
        let syntax = regex::Regex::new(
            r"`[^`\n]+`|--[A-Za-z][A-Za-z0-9_-]*|[A-Z][A-Z0-9]*_[A-Z0-9_]+|https?://[A-Za-z0-9_./?=&%#+:@~-]+",
        )
        .expect("CLI syntax pattern");
        assert!(
            source.len() > 100,
            "must measure the populated command tree"
        );
        for (locale, catalog) in TRANSLATIONS.iter() {
            assert_eq!(
                catalog.keys().collect::<Vec<_>>(),
                source.keys().collect::<Vec<_>>(),
                "{locale} help coverage"
            );
            assert!(
                catalog.values().all(|value| !value.trim().is_empty()),
                "{locale} empty help"
            );
            for (key, value) in catalog {
                let tokens = |text: &str| {
                    let mut tokens: Vec<_> = syntax
                        .find_iter(text)
                        .map(|found| found.as_str().to_owned())
                        .collect();
                    tokens.sort();
                    tokens
                };
                assert_eq!(
                    tokens(&source[key]),
                    tokens(value),
                    "{locale} changed CLI syntax in {key}"
                );
            }
            assert!(
                catalog[API_KEY_PROMPT].ends_with(' '),
                "{locale} credential prompt spacing"
            );
        }
    }

    #[test]
    fn every_catalog_keeps_console_record_field_identifiers() {
        let source: Catalog = serde_json::from_str(include_str!("localization/en.json")).unwrap();
        assert!(
            source.values().any(|text| text.contains("binding_id=")),
            "measure a known populated console record field"
        );
        for (locale, catalog) in TRANSLATIONS.iter() {
            field_contract::validate_fields(&source, catalog)
                .unwrap_or_else(|error| panic!("{locale}: {error}"));
        }
        field_contract::validate_fields(&source, &source).unwrap();
    }

    #[test]
    fn translated_console_record_keys_are_rejected_by_the_catalog_owner() {
        let source = Catalog::from([(
            "field".into(),
            "- binding_id={0} dispatcher_in_flight={1}".into(),
        )]);
        assert_eq!(
            field_contract::field_identifiers(&source["field"]),
            ["binding_id=", "dispatcher_in_flight="]
        );
        let mut translated = source.clone();
        translated.insert(
            "field".into(),
            "- identifiant_de_liaison={0} en_cours_du_dispatcher={1}".into(),
        );
        let error = field_contract::validate_fields(&source, &translated)
            .expect_err("a translated machine field must fail the same owning validator");
        assert!(error.contains("changed CLI record field identifiers"));
    }

    #[test]
    fn every_result_catalog_preserves_complete_rust_format_fields() {
        let source: Catalog =
            serde_json::from_str(include_str!("localization_formats.json")).unwrap();
        assert!(
            source.len() > 500,
            "measure the complete result-template registry"
        );
        let fields = regex::Regex::new(r"\{\{|\}\}|\{[^{}]*\}").unwrap();
        let tokens = |text: &str| {
            let mut tokens: Vec<_> = fields
                .find_iter(text)
                .map(|field| field.as_str().to_owned())
                .collect();
            tokens.sort();
            tokens
        };
        for (locale, catalog) in TRANSLATIONS.iter() {
            for (key, template) in &source {
                assert_eq!(
                    tokens(template),
                    tokens(&catalog[key]),
                    "{locale} changed formatting specifications in {key}"
                );
            }
        }
    }

    #[test]
    fn localized_help_keeps_parser_identifiers_and_behavior() {
        for locale in TRANSLATIONS.keys() {
            let mut command = command_for(locale);
            let help = command.render_long_help().to_string();
            assert!(help.contains("--help"));
            assert!(help.contains("--version"));
            assert!(help.contains(&TRANSLATIONS[locale]["Usage"]));
            assert!(command
                .try_get_matches_from(["harn", "run", "--help"])
                .is_err_and(|error| error.kind() == clap::error::ErrorKind::DisplayHelp));
        }
        let french = command_for("fr").render_long_help().to_string();
        assert!(french.contains("Utilisation"));
        assert!(!french.contains("Usage:"));
    }
}
