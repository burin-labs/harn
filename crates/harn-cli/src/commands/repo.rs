use crate::cli::RepoArgs;

pub(crate) fn run(args: RepoArgs) {
    if let Err(error) = measure(args) {
        eprintln!("repo loc: {error}");
        std::process::exit(2);
    }
}

#[cfg(not(feature = "hostlib"))]
fn measure(_: RepoArgs) -> Result<(), String> {
    Err("repository measurement requires the hostlib feature".into())
}

#[cfg(feature = "hostlib")]
fn measure(args: RepoArgs) -> Result<(), String> {
    let crate::cli::RepoCommand::Loc {
        directory,
        registry,
        json,
    } = args.command;
    let registry = registry.unwrap_or_else(|| directory.join("scripts/repo-loc.json"));
    let bytes =
        std::fs::read(&registry).map_err(|error| format!("{}: {error}", registry.display()))?;
    let policy = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let report =
        harn_hostlib::repo_loc::measure(&directory, &policy).map_err(|error| error.to_string())?;
    if json {
        println!(
            "{}",
            serde_json::to_string(&report).map_err(|error| error.to_string())?
        );
    } else {
        println!(
            "{} files: {} code, {} comment, {} blank lines",
            report.total.files, report.total.code, report.total.comment, report.total.blank
        );
        println!(
            "{} excluded paths, {} unsupported files, {} unmapped files; complete={}",
            report.excluded.len(),
            report.unsupported.len(),
            report.unmapped.len(),
            report.complete
        );
    }
    Ok(())
}
