//! Shared fixtures for the worker's test modules.
//!
//! `ScopedEnvVar` in particular is why the worker's tests serialize on
//! `ENV_LOCK`: process environment is global, so two tests configuring
//! different secret chains at once would read each other's setup.

use std::path::{Path, PathBuf};

pub(crate) static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) struct ScopedEnvVar {
    key: &'static str,
    previous: Option<String>,
}

impl ScopedEnvVar {
    pub(crate) fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for ScopedEnvVar {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

pub(crate) async fn write_script(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("worker.harn");
    tokio::fs::write(&path, body).await.expect("write script");
    path
}
