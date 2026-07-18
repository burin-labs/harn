pub(crate) fn reviewed_compile_receipt() -> serde_json::Value {
    serde_json::json!({
        "schema_version": "harn.persona.prompt_compile.v1",
        "ok": true,
        "prompt_digest": "sha256:prompt",
        "catalog_digest": "sha256:catalog",
        "checkpoint": {
            "status": "accepted",
            "attempts": 1,
            "repaired": false,
            "provider": "mock",
            "model": "mock",
        },
        "usage": {
            "input_tokens": 11,
            "output_tokens": 7,
            "total_tokens": 18,
            "realized_cost_usd": 0.0,
        },
        "blueprint": {
            "schema_version": "1",
            "name": "accepted_prompt_watch",
            "description": "Watches accepted prompt receipts.",
            "goal": "Prove the accepted receipt enters the canonical transaction.",
            "template": "deterministic-sweeper",
            "cron": {"cron": "0 9 * * *", "timezone": "UTC"},
        },
        "lowering": {
            "profile": "prompt_compiled_v1",
            "template": "deterministic-sweeper",
            "persona": {
                "name": "accepted_prompt_watch",
                "description": "Watches accepted prompt receipts.",
                "goal": "Prove the accepted receipt enters the canonical transaction.",
            },
            "policy": {
                "autonomy_tier": "suggest",
                "receipt_policy": "required",
            },
            "triggers": [{
                "id": "accepted_prompt_watch-cron",
                "kind": "cron",
                "provider": "cron",
                "events": ["cron.tick"],
                "secrets": {},
                "schedule": "0 9 * * *",
                "timezone": "UTC",
                "handler": "persona://accepted_prompt_watch",
            }],
        },
        "error": null,
    })
}
