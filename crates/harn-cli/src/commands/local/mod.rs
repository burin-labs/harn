//! `harn local` — first-class local LLM runtime lifecycle commands.
//!
//! Tracks issue #1599. Wraps Ollama, llama.cpp, MLX, and other
//! OpenAI-compatible local servers behind a stable command surface while
//! the underlying CLIs keep churning. Subcommands:
//!
//! - `list`   — survey every local provider Harn knows about
//! - `status` — show the active selection plus a brief summary of all
//! - `switch` — make a model the active local runtime (warm + persist)
//! - `stop`   — unload loaded models / stop Harn-managed servers
//!
//! All state lives under `<state_root>/local/` (see `super::local::state`).

pub(crate) mod list;
pub(crate) mod profile;
pub(crate) mod runtime;
pub(crate) mod state;
pub(crate) mod status;
pub(crate) mod stop;
pub(crate) mod switch;

use std::path::PathBuf;

use crate::cli::{LocalArgs, LocalCommand};

pub(crate) async fn run(args: LocalArgs) {
    let base_dir = current_base_dir();
    let result = match args.command {
        LocalCommand::List(args) => list::run(args, &base_dir).await,
        LocalCommand::Status(args) => status::run(args, &base_dir).await,
        LocalCommand::Switch(args) => switch::run(args, &base_dir).await,
        LocalCommand::Stop(args) => stop::run(args, &base_dir).await,
    };
    if let Err(error) = result {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

/// `harn local` reads/writes state under `<state_root>(cwd)/local/`. We
/// resolve `cwd` once here so subcommands don't each have to call
/// `std::env::current_dir()`.
fn current_base_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}
