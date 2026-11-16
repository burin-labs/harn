//! Stable context usage. A cumulative billed-session cost is unmeasured.

struct Context {
    call_id: String,
    used: u64,
    size: u64,
}

pub(super) struct PromptUsage {
    session_id: String,
}

impl PromptUsage {
    pub(super) fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
        }
    }

    pub(super) fn observe(
        &self,
        session_id: &str,
        call_id: &str,
        used: u64,
        size: Option<u64>,
    ) -> Option<serde_json::Value> {
        if session_id != self.session_id {
            return None;
        }
        let context = Context {
            call_id: call_id.to_string(),
            used,
            size: size.filter(|size| *size > 0)?,
        };
        Some(notification(&self.session_id, &context))
    }
}

fn notification(session_id: &str, context: &Context) -> serde_json::Value {
    serde_json::json!({
        "sessionId": session_id,
        "update": {
            "sessionUpdate": "usage_update", "used": context.used, "size": context.size,
            "cost": null,
            "_meta": {"harn": {
                "callId": context.call_id, "contextMeasurement": "request_estimate",
                "costAvailability": "unmeasured",
            }},
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_context_is_refused_and_unknown_capacity_never_becomes_zero() {
        let usage = PromptUsage::new("root-session");
        assert!(usage
            .observe("child", "child-call", 900, Some(1000))
            .is_none());
        let measured = usage
            .observe("root-session", "root-call", 17, Some(100))
            .unwrap();
        assert_eq!(measured["update"]["used"], 17);
        assert_eq!(measured["update"]["size"], 100);
        assert!(measured["update"]["cost"].is_null());
        assert!(usage.observe("child", "child-call", 900, None).is_none());
        let next_update = usage
            .observe("root-session", "next-root-call", 19, Some(200))
            .unwrap();
        assert_eq!(
            next_update["update"]["_meta"]["harn"]["callId"],
            "next-root-call"
        );
        assert!(
            next_update["update"]["cost"].is_null(),
            "unmeasured billed-session cost is not zero"
        );
        assert!(usage
            .observe("root-session", "unknown-model", 19, None)
            .is_none());
        assert!(usage
            .observe("root-session", "invalid-zero-size", 19, Some(0))
            .is_none());
    }
}
