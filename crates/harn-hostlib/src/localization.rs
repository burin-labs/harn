//! Shared CLI locale policy and help projection. Products own their wording
//! catalogs; this module owns selection, extraction, and rendering mechanics.

use std::collections::BTreeMap;

use clap::{Arg, Command};

/// Interface languages supported by Harn-powered command-line products.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Locale {
    /// English.
    English,
    /// Spanish.
    Spanish,
    /// French.
    French,
    /// German.
    German,
    /// Brazilian Portuguese.
    BrazilianPortuguese,
    /// Italian.
    Italian,
    /// Japanese.
    Japanese,
    /// Korean.
    Korean,
    /// Simplified Chinese.
    SimplifiedChinese,
    /// Russian.
    Russian,
}

impl Locale {
    /// The complete interface language set, in presentation order.
    pub const ALL: [Self; 10] = [
        Self::English,
        Self::Spanish,
        Self::French,
        Self::German,
        Self::BrazilianPortuguese,
        Self::Italian,
        Self::Japanese,
        Self::Korean,
        Self::SimplifiedChinese,
        Self::Russian,
    ];

    /// Stable BCP 47 identifier for catalogs, HTML, and saved preferences.
    pub const fn id(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Spanish => "es",
            Self::French => "fr",
            Self::German => "de",
            Self::BrazilianPortuguese => "pt-BR",
            Self::Italian => "it",
            Self::Japanese => "ja",
            Self::Korean => "ko",
            Self::SimplifiedChinese => "zh-Hans",
            Self::Russian => "ru",
        }
    }
}

/// Normalize BCP 47 or POSIX locale spelling. Unsupported Chinese scripts and
/// regions are refused rather than silently mapped to Simplified Chinese.
pub fn resolve_locale(value: &str) -> Option<Locale> {
    let normalized = value
        .split(['.', '@'])
        .next()?
        .replace('_', "-")
        .to_lowercase();
    let base = normalized.split('-').next()?;
    match base {
        "en" | "c" | "posix" => Some(Locale::English),
        "es" => Some(Locale::Spanish),
        "fr" => Some(Locale::French),
        "de" => Some(Locale::German),
        "pt" => Some(Locale::BrazilianPortuguese),
        "it" => Some(Locale::Italian),
        "ja" => Some(Locale::Japanese),
        "ko" => Some(Locale::Korean),
        "ru" => Some(Locale::Russian),
        "zh" => {
            let mut parts = normalized.split('-').skip(1).peekable();
            for _ in 0..3 {
                if parts.peek().is_some_and(|part| {
                    part.len() == 3 && part.bytes().all(|byte| byte.is_ascii_alphabetic())
                }) {
                    let _ = parts.next();
                } else {
                    break;
                }
            }
            let script = parts.next_if(|part| {
                part.len() == 4 && part.bytes().all(|byte| byte.is_ascii_alphabetic())
            });
            let region = parts.next_if(|part| {
                (part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_alphabetic()))
                    || (part.len() == 3 && part.bytes().all(|byte| byte.is_ascii_digit()))
            });
            match script {
                Some("hans") => Some(Locale::SimplifiedChinese),
                Some(_) => None,
                None if region.is_some_and(|region| matches!(region, "tw" | "hk" | "mo")) => None,
                None => Some(Locale::SimplifiedChinese),
            }
        }
        _ => None,
    }
}

/// Read the first nonempty LC_ALL, LC_MESSAGES, or LANG value. Unsupported
/// explicit selections fall back to English, without consulting lower priority
/// variables. The process environment is read without changing global state.
pub fn process_locale() -> Locale {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.is_empty())
        .as_deref()
        .and_then(resolve_locale)
        .unwrap_or(Locale::English)
}

/// Look up a product-owned English source message, falling back to its source.
pub fn translated_text(catalog: &BTreeMap<String, String>, source: &str) -> String {
    catalog
        .get(source)
        .cloned()
        .unwrap_or_else(|| source.to_owned())
}

fn translate_argument(mut argument: Arg, catalog: &BTreeMap<String, String>) -> Arg {
    if let Some(source) = argument.get_help().map(ToString::to_string) {
        argument = argument.help(translated_text(catalog, &source));
    }
    if let Some(source) = argument.get_long_help().map(ToString::to_string) {
        argument = argument.long_help(translated_text(catalog, &source));
    }
    let heading = argument
        .get_help_heading()
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if argument.get_index().is_some() {
                "Arguments"
            } else {
                "Options"
            }
            .to_owned()
        });
    argument.help_heading(translated_text(catalog, &heading))
}

/// Project translated human-readable help over the canonical Clap parser.
/// Argument identifiers, values, aliases, validation, and parser behavior are
/// retained. Empty catalogs return the original command without changing layout.
pub fn localize_command(mut command: Command, catalog: &BTreeMap<String, String>) -> Command {
    if catalog.is_empty() {
        return command;
    }
    command.build();
    macro_rules! translate_field {
        ($get:ident, $set:ident) => {
            if let Some(source) = command.$get().map(ToString::to_string) {
                command = command.$set(translated_text(catalog, &source));
            }
        };
    }
    translate_field!(get_about, about);
    translate_field!(get_long_about, long_about);
    translate_field!(get_before_help, before_help);
    translate_field!(get_before_long_help, before_long_help);
    translate_field!(get_after_help, after_help);
    translate_field!(get_after_long_help, after_long_help);
    translate_field!(get_subcommand_help_heading, subcommand_help_heading);
    if command.get_subcommand_help_heading().is_none() {
        command = command.subcommand_help_heading(translated_text(catalog, "Commands"));
    }
    command
        .help_template(format!(
            "{{before-help}}{{about-with-newline}}\n{}: {{usage}}\n\n{{all-args}}{{after-help}}",
            translated_text(catalog, "Usage"),
        ))
        .mut_args(|argument| translate_argument(argument, catalog))
        .mut_subcommands(|child| localize_command(child, catalog))
}

/// Extract the complete English help catalog from a product's live Clap tree.
/// This includes generated help/version descriptions and standard headings.
pub fn collect_help_messages(mut command: Command) -> BTreeMap<String, String> {
    command.build();
    let mut catalog = BTreeMap::new();
    for heading in ["Usage", "Commands", "Arguments", "Options"] {
        catalog.insert(heading.to_owned(), heading.to_owned());
    }
    collect(&command, &mut catalog);
    catalog
}

fn collect(command: &Command, catalog: &mut BTreeMap<String, String>) {
    for source in [
        command.get_about(),
        command.get_long_about(),
        command.get_before_help(),
        command.get_before_long_help(),
        command.get_after_help(),
        command.get_after_long_help(),
    ]
    .into_iter()
    .flatten()
    .map(ToString::to_string)
    {
        catalog.insert(source.clone(), source);
    }
    if let Some(source) = command.get_subcommand_help_heading() {
        catalog.insert(source.to_owned(), source.to_owned());
    }
    for argument in command.get_arguments() {
        for source in [argument.get_help(), argument.get_long_help()]
            .into_iter()
            .flatten()
            .map(ToString::to_string)
        {
            catalog.insert(source.clone(), source);
        }
        if let Some(source) = argument.get_help_heading() {
            catalog.insert(source.to_owned(), source.to_owned());
        }
    }
    for child in command.get_subcommands() {
        collect(child, catalog);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_locale_registry_resolves_its_own_ids_and_regions() {
        for locale in Locale::ALL {
            assert_eq!(resolve_locale(locale.id()), Some(locale));
        }
        assert_eq!(resolve_locale("fr_CA.UTF-8"), Some(Locale::French));
        assert_eq!(resolve_locale("pt_PT"), Some(Locale::BrazilianPortuguese));
        assert_eq!(resolve_locale("zh_Hant_TW"), None);
        assert_eq!(resolve_locale("C.UTF-8"), Some(Locale::English));
    }

    #[test]
    fn chinese_script_precedes_region_and_extensions_are_not_locale_fields() {
        for tag in [
            "zh-Hans-TW",
            "zh-Hans-HK",
            "zh-Hans-MO",
            "zh_Hans_TW.UTF-8",
            "zh-CN-x-hant",
            "zh-x-tw",
            "zh-u-rg-twzzzz",
            "zh-Hans-TW-x-hant",
        ] {
            assert_eq!(
                resolve_locale(tag),
                Some(Locale::SimplifiedChinese),
                "{tag}"
            );
        }
        for tag in [
            "zh-Hant-CN",
            "zh-Hant",
            "zh-TW-x-hans",
            "zh-HK",
            "zh-MO",
            "zh-Latn-CN",
        ] {
            assert_eq!(resolve_locale(tag), None, "{tag}");
        }
    }

    #[test]
    fn extraction_and_projection_keep_a_foreign_products_parser_contract() {
        let command = Command::new("workbench").subcommand(
            Command::new("open")
                .about("Open a project")
                .arg(Arg::new("path").long("path").help("Project path")),
        );
        let source = collect_help_messages(command.clone());
        assert!(source.contains_key("Project path"));
        assert!(source.contains_key("Print help"));
        let catalog = BTreeMap::from([
            ("Usage".to_owned(), "Utilisation".to_owned()),
            ("Open a project".to_owned(), "Ouvrir un projet".to_owned()),
            ("Project path".to_owned(), "Chemin du projet".to_owned()),
        ]);
        let mut projected = localize_command(command, &catalog);
        assert!(projected
            .render_long_help()
            .to_string()
            .contains("Ouvrir un projet"));
        let matches = projected
            .try_get_matches_from(["workbench", "open", "--path", "drafts"])
            .expect("foreign parser still accepts canonical flags");
        assert_eq!(
            matches
                .subcommand_matches("open")
                .unwrap()
                .get_one::<String>("path")
                .map(String::as_str),
            Some("drafts")
        );
    }
}
