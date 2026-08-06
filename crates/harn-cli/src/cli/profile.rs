use std::path::PathBuf;

use clap::Args;

#[derive(Clone, Debug, Default, Args)]
pub(crate) struct ProfileArgs {
    /// Print a categorical timing breakdown (LLM vs tools vs steps vs VM/residual).
    #[arg(
        long = "profile",
        env = "HARN_PROFILE",
        action = clap::ArgAction::SetTrue,
        value_parser = clap::builder::BoolishValueParser::new()
    )]
    pub text: bool,
    /// Write the profile rollup as JSON to the given path. Implies `--profile`.
    #[arg(long = "profile-json", env = "HARN_PROFILE_JSON", value_name = "PATH")]
    pub json_path: Option<PathBuf>,
}
