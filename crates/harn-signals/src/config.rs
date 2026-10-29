//! `config.toml` in the inbox directory: lease timing, retention, and the
//! delivery policy every consumer of the inbox shares.
//!
//! Muting, minimum severity, and quiet hours live here and are applied by the
//! inbox when it routes, so every harness, pipeline, and host reading this
//! inbox agrees on what is held. Hosts project their own settings into this
//! file; they do not filter reminders after leasing them.

use std::path::Path;

use serde::Deserialize;
use time::{OffsetDateTime, Time, UtcOffset};

use crate::model::{Severity, Signal};
use crate::SignalsError;

pub const CONFIG_FILE: &str = "config.toml";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// A session not seen for this long is dropped from routing.
    pub stale_after: time::Duration,
    /// Resolved and expired signals are pruned after this long.
    pub retain_for: time::Duration,
    /// A lease not acked within this long may be redelivered, and a session
    /// silent this long loses its unacked reminders to a responsive session.
    pub lease_for: time::Duration,
    /// A fired signal that is not fired again within this long expires, so an
    /// alert nobody resolves cannot stay pending forever.
    pub fired_ttl: time::Duration,
    /// The most signals the inbox keeps. Settled signals are pruned oldest
    /// first; past that, firing a new key is refused.
    pub max_signals: usize,
    /// Sessions with this role receive signals nothing else matches.
    pub fallback_role: String,
    pub policy: Policy,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            stale_after: time::Duration::hours(2),
            retain_for: time::Duration::days(7),
            lease_for: time::Duration::minutes(10),
            fired_ttl: time::Duration::hours(24),
            max_signals: 1000,
            fallback_role: "control".to_string(),
            policy: Policy::default(),
        }
    }
}

/// Which signals the inbox holds back from routing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Signals below this severity are never routed.
    pub min_severity: Severity,
    /// Keys to hold. An entry ending in `*` matches every key with that prefix.
    pub muted: Vec<String>,
    pub quiet_hours: Option<QuietHours>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            min_severity: Severity::Info,
            muted: Vec::new(),
            quiet_hours: None,
        }
    }
}

/// A daily window in which only signals at or above `min_severity` route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuietHours {
    pub start: Time,
    pub end: Time,
    pub utc_offset: UtcOffset,
    pub min_severity: Severity,
}

impl QuietHours {
    pub fn contains(&self, now: OffsetDateTime) -> bool {
        let local = now.to_offset(self.utc_offset).time();
        if self.start <= self.end {
            self.start <= local && local < self.end
        } else {
            local >= self.start || local < self.end
        }
    }
}

/// Why the policy holds a signal back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    Muted,
    BelowMinimum,
    QuietHours,
}

impl Hold {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Muted => "muted",
            Self::BelowMinimum => "below minimum severity",
            Self::QuietHours => "quiet hours",
        }
    }
}

impl Policy {
    pub fn hold(&self, signal: &Signal, now: OffsetDateTime) -> Option<Hold> {
        if self.muted.iter().any(|pattern| {
            let matches = |key: &str| match pattern.strip_suffix('*') {
                Some(prefix) => key.starts_with(prefix),
                None => key == pattern,
            };
            matches(&signal.key)
                || signal
                    .capture
                    .as_ref()
                    .is_some_and(|capture| matches(&capture.monitor_key))
        }) {
            return Some(Hold::Muted);
        }
        if signal.severity < self.min_severity {
            return Some(Hold::BelowMinimum);
        }
        if let Some(quiet) = &self.quiet_hours {
            if quiet.contains(now) && signal.severity < quiet.min_severity {
                return Some(Hold::QuietHours);
            }
        }
        None
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    stale_after_seconds: Option<u32>,
    retain_for_seconds: Option<u32>,
    lease_seconds: Option<u32>,
    fired_ttl_seconds: Option<u32>,
    max_signals: Option<usize>,
    fallback_role: Option<String>,
    min_severity: Option<Severity>,
    #[serde(default)]
    muted: Vec<String>,
    quiet_hours: Option<QuietHoursFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuietHoursFile {
    start: String,
    end: String,
    #[serde(default)]
    utc_offset: Option<String>,
    #[serde(default)]
    min_severity: Option<Severity>,
}

impl Config {
    pub(crate) fn load(dir: &Path) -> Result<Self, SignalsError> {
        let path = dir.join(CONFIG_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(source) => return Err(SignalsError::io(&path, source)),
        };
        Self::parse(&text).map_err(|message| SignalsError::Config { path, message })
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let file: ConfigFile = toml::from_str(text).map_err(|error| error.to_string())?;
        let defaults = Self::default();
        let seconds = |value: Option<u32>, default: time::Duration| {
            value.map_or(default, |seconds| {
                time::Duration::seconds(i64::from(seconds))
            })
        };
        let lease_for = seconds(file.lease_seconds, defaults.lease_for);
        if lease_for <= time::Duration::ZERO {
            return Err("lease_seconds must be positive".into());
        }
        let fired_ttl = seconds(file.fired_ttl_seconds, defaults.fired_ttl);
        if fired_ttl <= time::Duration::ZERO {
            return Err("fired_ttl_seconds must be positive".into());
        }
        let max_signals = file.max_signals.unwrap_or(defaults.max_signals);
        if max_signals == 0 {
            return Err("max_signals must be positive".into());
        }
        let quiet_hours = file
            .quiet_hours
            .map(|quiet| {
                Ok::<_, String>(QuietHours {
                    start: parse_clock(&quiet.start, "quiet_hours.start")?,
                    end: parse_clock(&quiet.end, "quiet_hours.end")?,
                    utc_offset: match quiet.utc_offset.as_deref() {
                        None => UtcOffset::UTC,
                        Some(text) => parse_offset(text)?,
                    },
                    min_severity: quiet.min_severity.unwrap_or(Severity::Urgent),
                })
            })
            .transpose()?;
        Ok(Self {
            stale_after: seconds(file.stale_after_seconds, defaults.stale_after),
            retain_for: seconds(file.retain_for_seconds, defaults.retain_for),
            lease_for,
            fired_ttl,
            max_signals,
            fallback_role: file.fallback_role.unwrap_or(defaults.fallback_role),
            policy: Policy {
                min_severity: file.min_severity.unwrap_or(Severity::Info),
                muted: file.muted,
                quiet_hours,
            },
        })
    }
}

fn parse_clock(text: &str, field: &str) -> Result<Time, String> {
    let invalid = || format!("{field} must be HH:MM, not `{text}`");
    let (hour, minute) = text.split_once(':').ok_or_else(invalid)?;
    let hour: u8 = hour.parse().map_err(|_| invalid())?;
    let minute: u8 = minute.parse().map_err(|_| invalid())?;
    Time::from_hms(hour, minute, 0).map_err(|_| invalid())
}

fn parse_offset(text: &str) -> Result<UtcOffset, String> {
    let invalid = || format!("quiet_hours.utc_offset must be +HH:MM or -HH:MM, not `{text}`");
    let (sign, rest) = if let Some(rest) = text.strip_prefix('+') {
        (1i8, rest)
    } else if let Some(rest) = text.strip_prefix('-') {
        (-1i8, rest)
    } else {
        return Err(invalid());
    };
    let (hours, minutes) = rest.split_once(':').ok_or_else(invalid)?;
    let hours: i8 = hours.parse().map_err(|_| invalid())?;
    let minutes: i8 = minutes.parse().map_err(|_| invalid())?;
    UtcOffset::from_hms(sign * hours, sign * minutes, 0).map_err(|_| invalid())
}
