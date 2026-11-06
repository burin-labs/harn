use clap::Args;
use std::path::PathBuf;

#[derive(Debug, Args)]
pub(crate) struct FlightRecorderArgs {
    /// Record the exact source-level VM code path to a bounded local file.
    /// Values, arguments, and results are never included.
    #[arg(hide_env_values = true,
        long = "flight-recorder",
        env = "HARN_FLIGHT_RECORDER",
        action = clap::ArgAction::SetTrue,
        value_parser = clap::builder::BoolishValueParser::new()
    )]
    pub enabled: bool,
    /// Write the flight recording to this file instead of the default under
    /// `.harn-runs/flight-recordings/`. Serve appends each execution's ID.
    #[arg(
        long = "flight-recorder-out",
        value_name = "PATH",
        requires = "enabled"
    )]
    pub out: Option<PathBuf>,
    /// Maximum exact VM events retained in the in-memory ring.
    #[arg(long = "flight-recorder-max-events", value_name = "COUNT",
        value_parser = clap::value_parser!(u32).range(1..=1_000_000), requires = "enabled")]
    pub max_events: Option<u32>,
    /// Number of newest default-location flight recordings to retain.
    #[arg(long = "flight-recorder-retain", value_name = "COUNT",
        value_parser = clap::value_parser!(u32).range(1..=1_024), requires = "enabled")]
    pub retain: Option<u32>,
}

impl FlightRecorderArgs {
    pub(crate) fn options(&self) -> harn_vm::flight_recorder::FlightRecorderOptions {
        let defaults = harn_vm::flight_recorder::FlightRecorderOptions::default();
        harn_vm::flight_recorder::FlightRecorderOptions {
            enabled: self.enabled,
            out: self.out.clone(),
            max_events: self
                .max_events
                .map_or(defaults.max_events, |value| value as usize),
            retain_files: self
                .retain
                .map_or(defaults.retain_files, |value| value as usize),
        }
    }
}
