use std::path::Path;

/// Keep a formatted file formatted after repairs, without reformatting an
/// unformatted file's untouched lines.
pub(super) fn keep_canonical_formatting(path: &Path, original: &str, edited: String) -> String {
    let options = project_options(path);
    if !harn_fmt::format_source_opts(original, &options)
        .is_ok_and(|formatted| formatted == original)
    {
        return edited;
    }
    harn_fmt::format_source_opts(&edited, &options).unwrap_or(edited)
}

pub(super) fn format_project_source(path: &Path, source: &str) -> Result<String, String> {
    harn_fmt::format_source_opts(source, &project_options(path))
        .map_err(|error| format!("failed to format repair output {}: {error}", path.display()))
}

fn project_options(path: &Path) -> harn_fmt::FmtOptions {
    let config = match harn_modules::project_config::load_for_path(path) {
        Ok(config) => config,
        Err(error) => {
            eprintln!(
                "warning: failed to load formatter config for {}: {error}; using defaults",
                path.display()
            );
            harn_modules::project_config::HarnConfig::default()
        }
    };
    let mut options = harn_fmt::FmtOptions::default();
    if let Some(line_width) = config.fmt.line_width {
        options.line_width = line_width;
    }
    if let Some(separator_width) = config.fmt.separator_width {
        options.separator_width = separator_width;
    }
    options
}
