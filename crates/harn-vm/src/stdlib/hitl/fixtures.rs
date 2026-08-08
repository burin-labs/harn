use super::*;
use crate::stdlib::host::dispatch_mock_host_call;

pub(super) async fn maybe_apply_mock_response(
    ctx: Option<&AsyncBuiltinCtx>,
    kind: HitlRequestKind,
    request_id: &str,
    request_payload: &JsonValue,
) -> Result<(), VmError> {
    maybe_apply_mock_response_with_harness(ctx, None, kind, request_id, request_payload).await
}

pub(super) async fn maybe_apply_mock_response_with_harness(
    ctx: Option<&AsyncBuiltinCtx>,
    harness: Option<&crate::harness::VmHarness>,
    kind: HitlRequestKind,
    request_id: &str,
    request_payload: &JsonValue,
) -> Result<(), VmError> {
    let mut params = request_payload
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|(key, value)| {
            (
                crate::value::intern_key(&key),
                crate::stdlib::json_to_vm_value(&value),
            )
        })
        .collect::<crate::value::DictMap>();
    params.put_str("request_id", request_id);
    let fixture_method = match kind {
        HitlRequestKind::Question => "question_response",
        HitlRequestKind::Approval => "approval_response",
        HitlRequestKind::DualControl => "dual_control_response",
        HitlRequestKind::Escalation => "escalation_response",
    };
    let fixture_result = harness
        .and_then(|root| {
            root.inner().fixtures().dispatch(
                harn_builtin_meta::CapabilityId::Interaction,
                fixture_method,
                &[VmValue::dict(params.clone())],
            )
        })
        .or_else(|| {
            ctx.and_then(|ctx| ctx.child_vm().root_harness_value())
                .and_then(|root| match root {
                    VmValue::Harness(root) => root.inner().fixtures().dispatch(
                        harn_builtin_meta::CapabilityId::Interaction,
                        fixture_method,
                        &[VmValue::dict(params.clone())],
                    ),
                    _ => None,
                })
        });
    let result = fixture_result.or_else(|| dispatch_mock_host_call("hitl", kind.as_str(), &params));
    let Some(result) = result else {
        return Ok(());
    };
    let value = result?;
    let responses = match value {
        VmValue::List(items) => items.iter().cloned().collect::<Vec<_>>(),
        other => vec![other],
    };
    for response in responses {
        let response_dict = response.as_dict().ok_or_else(|| {
            VmError::Runtime(format!(
                "mocked HITL {} response must be a dict or list<dict>",
                kind.as_str()
            ))
        })?;
        let hitl_response = parse_hitl_response_dict(request_id, response_dict)?;
        append_hitl_response(None, hitl_response)
            .await
            .map_err(VmError::Runtime)?;
    }
    Ok(())
}
