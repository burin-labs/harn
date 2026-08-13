use super::*;

impl AcpServer {
    pub(super) fn handle_plan_document_mutation(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let request: AcpPlanDocumentMutationParams = match serde_json::from_value(params.clone()) {
            Ok(request) => request,
            Err(error) => {
                self.send_error(
                    id,
                    -32602,
                    &format!("invalid {ACP_METHOD_SESSION_PLAN_DOCUMENT_MUTATE} params: {error}"),
                );
                return;
            }
        };
        let Some(session_id) = self
            .restored_session_id(id, params, ACP_METHOD_SESSION_PLAN_DOCUMENT_MUTATE)
            .map(str::to_owned)
        else {
            return;
        };
        if self
            .sessions
            .get(&session_id)
            .is_some_and(|session| session.host_bridge.is_some())
        {
            self.send_error(
                id,
                ACP_PLAN_MUTATION_BUSY_CODE,
                "plan documents cannot be mutated during an active prompt",
            );
            return;
        }

        let author_id = self
            .authenticated_principal
            .as_ref()
            .map(|principal| principal.subject.trim())
            .filter(|subject| !subject.is_empty())
            .unwrap_or(crate::auth::ANONYMOUS_SUBJECT)
            .to_string();
        let created_at = harn_vm::llm::plan::plan_timestamp();
        let event_id = harn_vm::llm::plan::new_plan_event_id();
        let result =
            mutate_plan_document(&session_id, &request, &author_id, &created_at, &event_id);
        let event = match result {
            Ok(event) => event,
            Err(harn_vm::llm::plan::PlanDocumentError::Conflict {
                document_id,
                expected_revision_id,
                current_revision_id,
            }) => {
                self.send_error_with_data(
                    id,
                    ACP_PLAN_REVISION_CONFLICT_CODE,
                    "collaborative plan revision conflict",
                    serde_json::json!({
                        "schemaVersion": ACP_PLAN_REVISION_CONFLICT_SCHEMA,
                        "documentId": document_id,
                        "expectedRevisionId": expected_revision_id,
                        "currentRevisionId": current_revision_id,
                    }),
                );
                return;
            }
            Err(error) => {
                self.send_error(id, -32602, &error.to_string());
                return;
            }
        };

        if let Err(error) = harn_vm::llm::plan::persist_plan_document_event(&session_id, &event) {
            self.send_error(
                id,
                -32000,
                &format!("failed to persist collaborative plan mutation: {error}"),
            );
            return;
        }

        // Client mutations are accepted only while idle, so the server can
        // safely replace the prior turn's closed sinks with one durable sink
        // and this transport's live ACP projection.
        clear_session_sinks(&session_id);
        harn_vm::agent_sessions::register_event_log_sink(&session_id);
        register_sink(
            session_id.clone(),
            Arc::new(AcpAgentEventSink::new(self.output.clone())),
        );
        harn_vm::agent_events::emit_event(
            &harn_vm::agent_events::AgentEvent::PlanDocumentUpdated {
                session_id,
                event: Box::new(event.clone()),
            },
        );
        self.send_response(
            id,
            serde_json::to_value(AcpPlanDocumentMutationResult {
                plan_document: event.document().clone(),
            })
            .expect("plan mutation result serializes"),
        );
    }
}

fn mutate_plan_document(
    session_id: &str,
    request: &AcpPlanDocumentMutationParams,
    author_id: &str,
    created_at: &str,
    event_id: &str,
) -> Result<harn_vm::llm::plan::PlanDocumentEvent, harn_vm::llm::plan::PlanDocumentError> {
    use harn_vm::llm::plan::{
        AddPlanComment, ChangePlanCommentState, EditPlanDocument, PlanApprovalState, PlanAuthor,
        PlanDocumentError, PlanSource,
    };

    let mut store = harn_vm::llm::plan::resume_plan_document_store(session_id)?;
    if store.current().document_id != request.document_id {
        return Err(PlanDocumentError::Invalid(format!(
            "plan document {} does not match current document {}",
            request.document_id,
            store.current().document_id
        )));
    }
    let author = PlanAuthor {
        id: author_id.to_string(),
        display_name: None,
    };
    let source = PlanSource {
        kind: "acp_client".to_string(),
        uri: None,
    };
    match &request.mutation {
        AcpPlanDocumentMutation::Edit { markdown, plan } => {
            let plan = plan
                .clone()
                .unwrap_or_else(|| store.current().current_revision.plan.clone());
            store.edit(EditPlanDocument {
                expected_revision_id: request.expected_revision_id.clone(),
                markdown: markdown.clone(),
                plan,
                author,
                source,
                created_at: created_at.to_string(),
                event_id: event_id.to_string(),
            })?;
        }
        AcpPlanDocumentMutation::AddComment {
            comment_id,
            anchor,
            body,
        } => {
            store.add_comment(AddPlanComment {
                expected_revision_id: request.expected_revision_id.clone(),
                comment_id: comment_id
                    .clone()
                    .unwrap_or_else(harn_vm::llm::plan::new_plan_comment_id),
                anchor: anchor.clone(),
                body: body.clone(),
                author,
                created_at: created_at.to_string(),
                event_id: event_id.to_string(),
            })?;
        }
        AcpPlanDocumentMutation::ChangeCommentState {
            comment_id,
            state,
            agent_run_id,
            explanation,
        } => {
            store.change_comment_state(ChangePlanCommentState {
                expected_revision_id: request.expected_revision_id.clone(),
                comment_id: comment_id.clone(),
                state: state.clone(),
                author,
                source,
                created_at: created_at.to_string(),
                event_id: event_id.to_string(),
                agent_run_id: agent_run_id.clone(),
                explanation: explanation.clone(),
            })?;
        }
        AcpPlanDocumentMutation::Approve { reviewer, reason } => {
            let mut plan = store.current().current_revision.plan.clone();
            let markdown = store.current().current_revision.markdown.clone();
            plan.approval.state = PlanApprovalState::Approved;
            plan.approval.reviewer =
                Some(reviewer.clone().unwrap_or_else(|| author_id.to_string()));
            plan.approval.approved_at = Some(created_at.to_string());
            plan.approval.reason = reason.clone();
            store.edit(EditPlanDocument {
                expected_revision_id: request.expected_revision_id.clone(),
                markdown,
                plan,
                author,
                source: PlanSource {
                    kind: "acp_approval".to_string(),
                    uri: None,
                },
                created_at: created_at.to_string(),
                event_id: event_id.to_string(),
            })?;
        }
    }
    Ok(store
        .events()
        .last()
        .expect("every plan mutation appends an event")
        .clone())
}
