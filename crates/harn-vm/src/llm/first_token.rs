pub(crate) const FIRST_TOKEN_METADATA_KEY: &str = "first_token_ms";

pub(crate) struct FirstTokenTimer {
    span_id: Option<u64>,
    started_at: tokio::time::Instant,
    recorded: bool,
}

impl FirstTokenTimer {
    pub(crate) fn for_current_span() -> Self {
        Self {
            span_id: crate::tracing::current_span_id(),
            started_at: tokio::time::Instant::now(),
            recorded: false,
        }
    }

    pub(crate) fn observe_delta(&mut self) {
        if self.recorded {
            return;
        }
        self.recorded = true;
        let first_token_ms = duration_ms(self.started_at.elapsed());
        if let Some(span_id) = self.span_id {
            crate::tracing::span_attach_metadata_if_absent(
                span_id,
                FIRST_TOKEN_METADATA_KEY,
                serde_json::json!(first_token_ms),
            );
        }
    }
}

pub(crate) fn duration_ms(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
