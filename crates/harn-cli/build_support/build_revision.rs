const BUILD_REVISION_ENV: &str = "HARN_BUILD_REVISION";

pub fn emit() {
    println!("cargo:rerun-if-env-changed={BUILD_REVISION_ENV}");

    let raw = std::env::var(BUILD_REVISION_ENV).ok();
    let revision = normalize(raw.as_deref())
        .unwrap_or_else(|message| panic!("{BUILD_REVISION_ENV} {message}"));
    println!(
        "cargo:rustc-env={BUILD_REVISION_ENV}={}",
        revision.unwrap_or_default()
    );
}

pub fn normalize(raw: Option<&str>) -> Result<Option<&str>, &'static str> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let revision = raw.trim();
    if revision.is_empty() {
        return Ok(None);
    }
    if !matches!(revision.len(), 40 | 64)
        || !revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("must be a full lowercase 40- or 64-character hexadecimal object ID");
    }
    Ok(Some(revision))
}
