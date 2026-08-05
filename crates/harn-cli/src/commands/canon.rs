//! `harn canon` exposes harn-canon packs without copying their routing or
//! predicate semantics into Rust. The shim only parses stable CLI flags and
//! hands a plan to the embedded Harn handler.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::cli::{CanonArgs, CanonCheckArgs, CanonCommand};
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

const CANON_CHECK_PLAN_ENV: &str = "HARN_CANON_CHECK_PLAN_JSON";

pub(crate) async fn run(args: CanonArgs) {
    let exit_code = match args.command {
        CanonCommand::Check(args) => check(args).await,
    };
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

async fn check(args: CanonCheckArgs) -> i32 {
    let plan = CanonCheckPlan::from_args(&args);
    let plan_json = match serde_json::to_string(&plan) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("canon: failed to serialize check plan: {error}");
            return 70;
        }
    };

    let _plan = ScopedEnvVar::set(CANON_CHECK_PLAN_ENV, &plan_json);
    dispatch::dispatch_to_embedded_script_no_sandbox("canon/check", Vec::new(), args.json).await
}

#[derive(Serialize)]
struct CanonCheckPlan {
    schema_version: u32,
    workspace_root: String,
    canon_root: Option<String>,
    paths: Vec<String>,
    pack_ids: Vec<String>,
    include_missing: bool,
    include_semantic: bool,
    budget_ms: u64,
    advisory: bool,
    feedback_header: String,
}

impl CanonCheckPlan {
    fn from_args(args: &CanonCheckArgs) -> Self {
        Self {
            schema_version: 1,
            workspace_root: path_string(&absolute_path(&args.workspace_root)),
            canon_root: args
                .canon_root
                .as_ref()
                .map(|path| path_string(&absolute_path(path))),
            paths: args.paths.iter().map(|path| path_string(path)).collect(),
            pack_ids: args
                .packs
                .iter()
                .map(|pack| pack.trim().to_string())
                .filter(|pack| !pack.is_empty())
                .collect(),
            include_missing: args.include_missing,
            include_semantic: args.include_semantic,
            budget_ms: args.budget_ms,
            advisory: args.advisory,
            feedback_header: args.feedback_header.clone(),
        }
    }
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
