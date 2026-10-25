use harn_vm_core::value::VmValue;

use super::string_list_value;

pub(super) fn all() -> VmValue {
    VmValue::list(COUNTRIES.iter().map(country_value).collect())
}

pub(super) fn by_code(raw: &str) -> Option<VmValue> {
    country_by_code(raw).map(country_value)
}

#[derive(Clone, Copy)]
struct CountryMeta {
    code: &'static str,
    name: &'static str,
    timezones: &'static [&'static str],
    currency_code: &'static str,
    currency_name: &'static str,
    holiday_calendars: &'static [&'static str],
}

static COUNTRIES: &[CountryMeta] = &[
    CountryMeta {
        code: "AU",
        name: "Australia",
        timezones: &[
            "Australia/Perth",
            "Australia/Darwin",
            "Australia/Adelaide",
            "Australia/Brisbane",
            "Australia/Sydney",
            "Australia/Melbourne",
            "Australia/Hobart",
        ],
        currency_code: "AUD",
        currency_name: "Australian dollar",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "BR",
        name: "Brazil",
        timezones: &[
            "America/Noronha",
            "America/Belem",
            "America/Fortaleza",
            "America/Recife",
            "America/Araguaina",
            "America/Maceio",
            "America/Bahia",
            "America/Sao_Paulo",
            "America/Campo_Grande",
            "America/Cuiaba",
            "America/Manaus",
            "America/Porto_Velho",
            "America/Boa_Vista",
            "America/Rio_Branco",
            "America/Eirunepe",
        ],
        currency_code: "BRL",
        currency_name: "Brazilian real",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "CA",
        name: "Canada",
        timezones: &[
            "America/St_Johns",
            "America/Halifax",
            "America/Toronto",
            "America/Winnipeg",
            "America/Regina",
            "America/Edmonton",
            "America/Vancouver",
            "America/Whitehorse",
            "America/Yellowknife",
            "America/Iqaluit",
        ],
        currency_code: "CAD",
        currency_name: "Canadian dollar",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "CN",
        name: "China",
        timezones: &["Asia/Shanghai"],
        currency_code: "CNY",
        currency_name: "Chinese yuan",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "DE",
        name: "Germany",
        timezones: &["Europe/Berlin"],
        currency_code: "EUR",
        currency_name: "Euro",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "FR",
        name: "France",
        timezones: &["Europe/Paris"],
        currency_code: "EUR",
        currency_name: "Euro",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "GB",
        name: "United Kingdom",
        timezones: &["Europe/London"],
        currency_code: "GBP",
        currency_name: "Pound sterling",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "IN",
        name: "India",
        timezones: &["Asia/Kolkata"],
        currency_code: "INR",
        currency_name: "Indian rupee",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "JP",
        name: "Japan",
        timezones: &["Asia/Tokyo"],
        currency_code: "JPY",
        currency_name: "Japanese yen",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "MX",
        name: "Mexico",
        timezones: &[
            "America/Mexico_City",
            "America/Cancun",
            "America/Chihuahua",
            "America/Hermosillo",
            "America/Mazatlan",
            "America/Tijuana",
        ],
        currency_code: "MXN",
        currency_name: "Mexican peso",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "NL",
        name: "Netherlands",
        timezones: &["Europe/Amsterdam"],
        currency_code: "EUR",
        currency_name: "Euro",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "NZ",
        name: "New Zealand",
        timezones: &["Pacific/Auckland", "Pacific/Chatham"],
        currency_code: "NZD",
        currency_name: "New Zealand dollar",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "SG",
        name: "Singapore",
        timezones: &["Asia/Singapore"],
        currency_code: "SGD",
        currency_name: "Singapore dollar",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "US",
        name: "United States",
        timezones: &[
            "America/New_York",
            "America/Chicago",
            "America/Denver",
            "America/Phoenix",
            "America/Los_Angeles",
            "America/Anchorage",
            "Pacific/Honolulu",
        ],
        currency_code: "USD",
        currency_name: "United States dollar",
        holiday_calendars: &["US-FEDERAL"],
    },
    CountryMeta {
        code: "ZA",
        name: "South Africa",
        timezones: &["Africa/Johannesburg"],
        currency_code: "ZAR",
        currency_name: "South African rand",
        holiday_calendars: &[],
    },
];

fn country_by_code(raw: &str) -> Option<&'static CountryMeta> {
    let code = raw.trim().to_ascii_uppercase();
    COUNTRIES.iter().find(|country| country.code == code)
}

fn country_value(country: &CountryMeta) -> VmValue {
    let default_timezone = if country.timezones.len() == 1 {
        VmValue::string(country.timezones[0])
    } else {
        VmValue::Nil
    };
    VmValue::dict([
        ("code", VmValue::string(country.code)),
        ("name", VmValue::string(country.name)),
        (
            "timezones",
            string_list_value(country.timezones.iter().copied()),
        ),
        ("default_timezone", default_timezone),
        (
            "default_timezone_ambiguous",
            VmValue::Bool(country.timezones.len() != 1),
        ),
        ("currency_code", VmValue::string(country.currency_code)),
        ("currency_name", VmValue::string(country.currency_name)),
        (
            "holiday_calendars",
            string_list_value(country.holiday_calendars.iter().copied()),
        ),
    ])
}
