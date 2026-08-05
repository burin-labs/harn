use crate::cli::{TraceArgs, TraceCommand, TraceImportArgs};
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

pub(crate) async fn handle(args: TraceArgs) -> Result<(), String> {
    match args.command {
        TraceCommand::Import(import) => run_import(import).await,
    }
}

async fn run_import(args: TraceImportArgs) -> Result<(), String> {
    let _file = ScopedEnvVar::set("HARN_TRACE_FILE", &args.trace_file);
    let _out = ScopedEnvVar::set("HARN_TRACE_OUTPUT", &args.output);
    let _id_guard = args
        .trace_id
        .as_deref()
        .map(|id| ScopedEnvVar::set("HARN_TRACE_ID", id));
    let exit = dispatch::dispatch_to_embedded_script(
        "trace_import",
        Vec::new(),
        /* json_mode */ false,
    )
    .await;
    if exit != 0 {
        return Err(format!("trace import exited with code {exit}"));
    }
    Ok(())
}
