//! The request context matched to the first successful actor response in a run.

#[derive(Clone, serde::Serialize)]
pub(crate) struct ActorRequestReceipt {
    schema: &'static str,
    pub(crate) session_id: String,
    pub(crate) run_id: String,
    call_id: String,
    call_role: &'static str,
    system_prompt_content_hash: String,
    context_manifest_content_hash: String,
    context_manifest: serde_json::Value,
}

impl ActorRequestReceipt {
    pub(crate) fn from_request(
        session_id: Option<&str>,
        call_id: &str,
        served_context: &serde_json::Value,
        manifest: &crate::llm::prompt::ContextAssemblyManifest,
    ) -> Option<Self> {
        if manifest.call_role() != "agent.main" {
            return None;
        }
        let run = crate::runtime_context::current_agent_run_ref()?;
        if Some(run.session_id.as_str()) != session_id {
            return None;
        }
        Some(Self {
            schema: "harn.agent.actor_request.v1",
            session_id: run.session_id,
            run_id: run.run_id,
            call_id: call_id.to_owned(),
            call_role: "agent.main",
            system_prompt_content_hash: served_context
                .get("system_prompt_content_hash")?
                .as_str()?
                .to_owned(),
            context_manifest_content_hash: served_context
                .get("manifest_content_hash")?
                .as_str()?
                .to_owned(),
            context_manifest: manifest.as_json(),
        })
    }
}
