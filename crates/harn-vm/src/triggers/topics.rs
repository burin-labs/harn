pub const TRIGGER_INBOX_LEGACY_TOPIC: &str = "trigger.inbox";
pub const TRIGGER_INBOX_CLAIMS_TOPIC: &str = "trigger.inbox.claims";
pub const TRIGGER_INBOX_ENVELOPES_TOPIC: &str = "trigger.inbox.envelopes";
pub const TRIGGER_OUTBOX_TOPIC: &str = "trigger.outbox";
pub const TRIGGER_ATTEMPTS_TOPIC: &str = "trigger.attempts";
pub const TRIGGER_DLQ_TOPIC: &str = "trigger.dlq";
pub const TRIGGER_CANCEL_REQUESTS_TOPIC: &str = "trigger.cancel.requests";
pub const TRIGGER_OPERATION_AUDIT_TOPIC: &str = "trigger.operations.audit";
pub const TRIGGERS_LIFECYCLE_TOPIC: &str = "triggers.lifecycle";

pub fn classify_trigger_dlq_error(error: &str) -> &'static str {
    let error = error.to_ascii_lowercase();
    if error.contains("budget")
        && (error.contains("exhaust")
            || error.contains("exceeded")
            || error.contains("limit")
            || error.contains("short-circuit"))
    {
        return "budget_exhausted";
    }
    if error.contains("timeout")
        || error.contains("timed out")
        || error.contains("deadline")
        || error.contains("wall-clock")
        || error.contains("wall clock")
    {
        return "handler_timeout";
    }
    if error.contains("401")
        || error.contains("403")
        || error.contains("unauthorized")
        || error.contains("forbidden")
        || error.contains("auth failed")
        || error.contains("authentication")
        || error.contains("permission denied")
    {
        return "auth_failed";
    }
    if error.contains("provider")
        && (error.contains("5xx")
            || error.contains("500")
            || error.contains("502")
            || error.contains("503")
            || error.contains("504")
            || error.contains("upstream")
            || error.contains("service unavailable"))
    {
        return "provider_5xx";
    }
    if error.contains("predicate")
        && (error.contains("panic")
            || error.contains("crash")
            || error.contains("thrown")
            || error.contains("exception")
            || error.contains("vmerror::thrown"))
    {
        return "predicate_panic";
    }
    if error.contains("handler")
        && (error.contains("panic")
            || error.contains("crash")
            || error.contains("thrown")
            || error.contains("exception")
            || error.contains("vmerror::thrown"))
    {
        return "handler_panic";
    }
    if error.contains("vmerror::thrown") || error.contains("handler threw") {
        return "handler_panic";
    }
    "unknown"
}

#[cfg(test)]
mod tests {
    use super::classify_trigger_dlq_error;

    #[test]
    fn classify_trigger_dlq_error_maps_operational_classes() {
        assert_eq!(
            classify_trigger_dlq_error("provider returned 503 service unavailable"),
            "provider_5xx"
        );
        assert_eq!(
            classify_trigger_dlq_error("predicate VmError::Thrown: nope"),
            "predicate_panic"
        );
        assert_eq!(
            classify_trigger_dlq_error("handler timed out after wall-clock deadline"),
            "handler_timeout"
        );
        assert_eq!(
            classify_trigger_dlq_error("downstream auth failed with 401"),
            "auth_failed"
        );
        assert_eq!(
            classify_trigger_dlq_error("trigger budget exhausted"),
            "budget_exhausted"
        );
        assert_eq!(classify_trigger_dlq_error("boom"), "unknown");
    }
}
