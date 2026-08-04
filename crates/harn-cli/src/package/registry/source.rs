//! The `source = "..."` URI shapes a lock entry can carry, and the blocking
//! HTTP retrieval of a registry index or a package archive behind them.

use crate::net;

use crate::package::*;

pub(crate) fn path_source_uri(path: &Path) -> Result<String, PackageError> {
    let url = Url::from_file_path(path)
        .map_err(|_| format!("failed to convert {} to file:// URL", path.display()))?;
    Ok(format!("path+{url}"))
}

pub(crate) fn path_from_source_uri(source: &str) -> Result<PathBuf, PackageError> {
    let raw = source
        .strip_prefix("path+")
        .ok_or_else(|| format!("invalid path source: {source}"))?;
    if crate::format::looks_like_windows_drive_path(raw) {
        return Ok(PathBuf::from(raw));
    }
    if let Ok(url) = Url::parse(raw) {
        return url
            .to_file_path()
            .map_err(|_| PackageError::Registry(format!("invalid file:// path source: {source}")));
    }
    Ok(PathBuf::from(raw))
}

pub(crate) fn archive_url_from_source_uri(source: &str) -> Result<&str, PackageError> {
    source
        .strip_prefix("archive+")
        .ok_or_else(|| format!("invalid archive source: {source}").into())
}

pub(crate) fn archive_source_uri(raw: &str) -> Result<String, PackageError> {
    Ok(format!("archive+{}", normalize_archive_url(raw)?))
}

pub(crate) fn registry_file_url_or_path(raw: &str) -> Result<Option<PathBuf>, PackageError> {
    if crate::format::looks_like_windows_drive_path(raw) {
        return Ok(Some(PathBuf::from(raw)));
    }
    if let Ok(url) = Url::parse(raw) {
        if url.scheme() == "file" {
            return url.to_file_path().map(Some).map_err(|_| {
                PackageError::Registry(format!("invalid file:// registry URL: {raw}"))
            });
        }
        return Ok(None);
    }
    Ok(Some(PathBuf::from(raw)))
}

pub(crate) fn normalize_archive_url(raw: &str) -> Result<String, PackageError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("archive URL cannot be empty".to_string().into());
    }
    if crate::format::looks_like_windows_drive_path(trimmed) {
        return normalize_archive_path(trimmed);
    }
    if let Ok(url) = Url::parse(trimmed) {
        return match url.scheme() {
            "file" => {
                let path = url.to_file_path().map_err(|_| {
                    PackageError::Registry(format!(
                        "invalid file:// package archive URL: {trimmed}"
                    ))
                })?;
                if path.exists() {
                    let canonical = path.canonicalize().map_err(|error| {
                        format!("failed to canonicalize {}: {error}", path.display())
                    })?;
                    let url = Url::from_file_path(canonical)
                        .map_err(|_| format!("failed to convert {trimmed} to file:// URL"))?;
                    Ok(url.to_string())
                } else {
                    Ok(url.to_string())
                }
            }
            "http" | "https" => Ok(url.to_string()),
            other => Err(format!("unsupported package archive URL scheme: {other}").into()),
        };
    }

    normalize_archive_path(trimmed)
}

fn normalize_archive_path(raw: &str) -> Result<String, PackageError> {
    let path = PathBuf::from(raw);
    if !path.exists() {
        return Err(format!("package archive not found: {}", path.display()).into());
    }
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("failed to canonicalize {}: {error}", path.display()))?;
    let url = Url::from_file_path(canonical)
        .map_err(|_| format!("failed to convert {raw} to file:// URL"))?;
    Ok(url.to_string())
}

fn package_registry_auth_token() -> Option<String> {
    std::env::var(HARN_PACKAGE_REGISTRY_TOKEN_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn apply_package_registry_auth(
    request: reqwest::blocking::RequestBuilder,
) -> reqwest::blocking::RequestBuilder {
    if let Some(token) = package_registry_auth_token() {
        request.bearer_auth(token)
    } else {
        request
    }
}

pub(crate) fn read_registry_source(source: &str) -> Result<String, PackageError> {
    if let Some(path) = registry_file_url_or_path(source)? {
        return fs::read_to_string(&path).map_err(|error| {
            PackageError::Registry(format!(
                "failed to read package registry {}: {error}",
                path.display()
            ))
        });
    }

    let url = Url::parse(source)
        .map_err(|error| format!("invalid package registry URL {source:?}: {error}"))?;
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(format!("unsupported package registry URL scheme: {other}").into()),
    }
    // `reqwest::blocking` builds its own current-thread tokio runtime and
    // panics if dropped from inside an already-running tokio runtime — which
    // is exactly what `harn add` / `harn install` do today. Hop onto a fresh
    // OS thread so the blocking client's lifetime is fully outside any
    // ambient runtime.
    let source_owned = source.to_string();
    std::thread::scope(|scope| {
        scope
            .spawn(move || fetch_registry_blocking(url, &source_owned))
            .join()
            .map_err(|_| PackageError::Registry("registry fetch thread panicked".to_string()))?
    })
}

fn fetch_registry_blocking(url: Url, source: &str) -> Result<String, PackageError> {
    let display_source = net::diagnostic_text(source);
    let client = net::blocking_http_client("cli.package.registry", Duration::from_secs(20))
        .map_err(PackageError::Registry)?;
    let response = apply_package_registry_auth(client.get(url))
        .send()
        .map_err(|error| {
            format!(
                "failed to fetch package registry {display_source}: {}",
                net::reqwest_error(&error)
            )
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("GET {display_source} returned HTTP {status}").into());
    }
    response.text().map_err(|error| {
        PackageError::Registry(format!(
            "failed to read package registry response: {}",
            net::reqwest_error(&error)
        ))
    })
}

pub(crate) fn read_package_archive_bytes(source: &str) -> Result<Vec<u8>, PackageError> {
    if let Some(path) = registry_file_url_or_path(source)? {
        let metadata = fs::metadata(&path).map_err(|error| {
            format!("failed to stat package archive {}: {error}", path.display())
        })?;
        if metadata.len() > PACKAGE_ARCHIVE_MAX_BYTES {
            return Err(format!(
                "package archive {} is {} bytes, above the {} byte limit",
                path.display(),
                metadata.len(),
                PACKAGE_ARCHIVE_MAX_BYTES
            )
            .into());
        }
        return fs::read(&path).map_err(|error| {
            PackageError::Registry(format!(
                "failed to read package archive {}: {error}",
                path.display()
            ))
        });
    }

    let url = Url::parse(source)
        .map_err(|error| format!("invalid package archive URL {source:?}: {error}"))?;
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(format!("unsupported package archive URL scheme: {other}").into()),
    }
    let source_owned = source.to_string();
    std::thread::scope(|scope| {
        scope
            .spawn(move || fetch_package_archive_blocking(url, &source_owned))
            .join()
            .map_err(|_| {
                PackageError::Registry("package archive fetch thread panicked".to_string())
            })?
    })
}

fn fetch_package_archive_blocking(url: Url, source: &str) -> Result<Vec<u8>, PackageError> {
    let display_source = net::diagnostic_text(source);
    let client = net::blocking_http_client("cli.package.archive", Duration::from_secs(30))
        .map_err(PackageError::Registry)?;
    let response = apply_package_registry_auth(client.get(url))
        .send()
        .map_err(|error| {
            format!(
                "failed to fetch package archive {display_source}: {}",
                net::reqwest_error(&error)
            )
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("GET {display_source} returned HTTP {status}").into());
    }
    if response
        .content_length()
        .is_some_and(|length| length > PACKAGE_ARCHIVE_MAX_BYTES)
    {
        return Err(format!(
            "package archive {display_source} is larger than the {PACKAGE_ARCHIVE_MAX_BYTES} byte limit"
        )
        .into());
    }
    let bytes = response.bytes().map_err(|error| {
        format!(
            "failed to read package archive response: {}",
            net::reqwest_error(&error)
        )
    })?;
    if bytes.len() as u64 > PACKAGE_ARCHIVE_MAX_BYTES {
        return Err(format!(
            "package archive {display_source} is larger than the {PACKAGE_ARCHIVE_MAX_BYTES} byte limit"
        )
        .into());
    }
    Ok(bytes.to_vec())
}
