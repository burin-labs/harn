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

    pub(crate) fn set_if_unset(key: &'static str, value: &str) -> Option<Self> {
        if std::env::var(key).is_ok() {
            None
        } else {
            Some(Self::set(key, value))
        }
    }
}

impl Drop for ScopedEnvVar {
    fn drop(&mut self) {
        if let Some(value) = &self.previous {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}
