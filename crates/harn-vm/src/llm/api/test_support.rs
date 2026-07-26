//! Shared test scaffolding for the `llm::api` test modules.
//!
//! `ScopedEnvVar` restores the previous value on drop, so a test that flips a
//! transport-gating variable cannot leak it into whatever nextest schedules
//! next on the same process.

pub(super) struct ScopedEnvVar {
    key: &'static str,
    previous: Option<String>,
}

impl ScopedEnvVar {
    pub(super) fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }

    pub(super) fn remove(key: &'static str) -> Self {
        let previous = std::env::var(key).ok();
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, previous }
    }
}

impl Drop for ScopedEnvVar {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => unsafe { std::env::set_var(self.key, value) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

pub(super) fn allow_stubbed_llm_transport() -> ScopedEnvVar {
    ScopedEnvVar::remove(crate::llm::LLM_CALLS_DISABLED_ENV)
}
