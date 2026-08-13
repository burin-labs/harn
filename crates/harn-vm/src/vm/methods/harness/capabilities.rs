impl crate::vm::Vm {
    /// Root-handle capability methods that bypass the null/mock-mode
    /// gating because they reshape the `Harness` value itself rather
    /// than touch the OS. Currently `with_net_policy` (issue #1913)
    /// and `is_quarantined`. Adding new entries here requires the
    /// caller-side allowlist in `call_harness_method` to be updated
    /// at the same time.
    async fn call_harness_root_capability_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        match method {
            "with_net_policy" => {
                let policy_value = args.first().ok_or_else(|| {
                    VmError::TypeError(
                        "Harness.with_net_policy expects a NetPolicy.create({...}) result"
                            .to_string(),
                    )
                })?;
                let dict = policy_value.as_dict().ok_or_else(|| {
                    VmError::TypeError(format!(
                        "Harness.with_net_policy: expected a NetPolicy dict, got {}",
                        policy_value.type_name()
                    ))
                })?;
                let policy = crate::harness_net::parse::policy_from_dict(dict)?;
                let source = crate::harness::Harness::from_inner(handle.inner().clone());
                Ok(source.with_net_policy(policy).into_vm_value())
            }
            "is_quarantined" => Ok(VmValue::Bool(handle.inner().is_quarantined())),
            _ => Err(method_unsupported(handle, method)),
        }
    }

    /// Apply the per-harness `NetPolicy` (issue #1913) to a
    /// `harness.net.*` method call. Returns `Ok(None)` when no policy
    /// is bound; `Ok(Some(Allow))` for an allowed (possibly audited)
    /// request; `Ok(Some(Deny))` carrying the typed VmError when the
    /// request must be blocked.
    async fn evaluate_net_policy_for_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<Option<NetPolicyOutcome>, VmError> {
        let Some(policy) = handle.inner().net_policy().cloned() else {
            return Ok(None);
        };
        let contract = NetPolicyMethodContract::for_method(method).ok_or_else(|| {
            VmError::CategorizedError {
                message: format!(
                    "HarnessNet method `{method}` has no destination attenuation contract"
                ),
                category: ErrorCategory::ToolRejected,
            }
        })?;
        let Some(url) = contract.url(args) else {
            // Calls without a URL-addressed destination have no check at this
            // seam. For URL-addressed methods with a missing/non-string URL,
            // preserve the underlying method's authoritative argument error.
            return Ok(None);
        };
        // Bypass is honoured *after* the policy is bound so that
        // configuring a policy and forgetting to clear the env var
        // still leaves an audit trail.
        if harness_net::bypass_enabled() {
            let bypass_audit = NetPolicyAudit {
                method: method.to_string(),
                url: url.to_string(),
                host: String::new(),
                port: None,
                reason: "HARN_NET_POLICY_BYPASS set; policy not enforced".to_string(),
                outcome: "bypass",
                bypass: true,
                matched_rule: None,
            };
            record_audit(&bypass_audit).await;
            return Ok(Some(NetPolicyOutcome::Allow));
        }
        let decision = policy.evaluate(method, url)?;
        match decision {
            NetPolicyDecision::Allow { audited, audit } => {
                if audited {
                    if let Some(audit) = &audit {
                        record_audit(audit).await;
                    }
                }
                Ok(Some(NetPolicyOutcome::Allow))
            }
            NetPolicyDecision::Deny { audit, quarantine } => {
                if quarantine {
                    handle.inner().mark_quarantined();
                }
                // Custom callback resolution: invoke the user closure
                // and respect its returned outcome string.
                if let OnViolation::Callback(closure) = policy.on_violation.clone() {
                    let request = violation_request_value(&audit);
                    match self.call_closure_pub(&closure, &[request]).await {
                        Ok(value) => match value {
                            VmValue::String(s) => {
                                let outcome = OnViolation::parse_str(s.as_str())?;
                                return self.apply_callback_outcome(handle, audit, outcome).await;
                            }
                            other => {
                                return Err(VmError::TypeError(format!(
                                    "NetPolicy.on_violation callback must return one of `error`, `audit_only`, `quarantine`, got {}",
                                    other.type_name()
                                )));
                            }
                        },
                        Err(err) => return Err(err),
                    }
                }
                record_audit(&audit).await;
                Ok(Some(NetPolicyOutcome::Deny(violation_vm_error(&audit))))
            }
        }
    }

    async fn apply_callback_outcome(
        &mut self,
        handle: &VmHarness,
        mut audit: NetPolicyAudit,
        outcome: OnViolation,
    ) -> Result<Option<NetPolicyOutcome>, VmError> {
        match outcome {
            OnViolation::Error => {
                audit.outcome = "error";
                record_audit(&audit).await;
                Ok(Some(NetPolicyOutcome::Deny(violation_vm_error(&audit))))
            }
            OnViolation::AuditOnly => {
                audit.outcome = "audit_only";
                record_audit(&audit).await;
                Ok(Some(NetPolicyOutcome::Allow))
            }
            OnViolation::Quarantine => {
                audit.outcome = "quarantine";
                handle.inner().mark_quarantined();
                record_audit(&audit).await;
                Ok(Some(NetPolicyOutcome::Deny(violation_vm_error(&audit))))
            }
            OnViolation::Callback(_) => Err(VmError::TypeError(
                "NetPolicy.on_violation callback may not return another callback".to_string(),
            )),
        }
    }

    fn call_harness_system_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        _args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        use crate::harness_system as sys;
        let json = match method {
            "cpu" => sys::cpu_snapshot(),
            "memory" => sys::memory_snapshot(),
            "gpus" | "gpu" => sys::gpus_snapshot(),
            "temperature" => sys::temperature_snapshot(),
            "platform" => sys::platform_snapshot(),
            "identity" => sys::identity_snapshot(),
            "processes" => sys::processes_snapshot(),
            _ => return Err(method_unsupported(handle, method)),
        };
        Ok(crate::stdlib::json_to_vm_value(&json))
    }

    async fn call_harness_secrets_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        let provider = crate::connectors::harn_module::active_harn_connector_ctx()
            .map(|ctx| ctx.secrets)
            .or_else(|| handle.inner().secret_provider().cloned())
            .ok_or_else(|| VmError::CategorizedError {
                message: "HarnessSecrets: no secret provider bound to this harness".to_string(),
                category: ErrorCategory::NotFound,
            })?;
        match method {
            "read" | "read_bytes" => {
                if args.len() > 2 {
                    return Err(VmError::TypeError(format!(
                        "{}.{method} expects name and optional scope",
                        handle.type_name()
                    )));
                }
                let name = secret_name_arg(handle, method, args.first())?;
                let scope = secret_scope_arg(args.get(1))?;
                let id = secret_id_for_scope(&name, &scope)?;
                crate::secrets::ensure_scoped_secret_access_allowed(method, &id)
                    .map_err(secret_error_to_vm)?;
                let secret = provider
                    .read_scoped(crate::secrets::SecretReadRequest {
                        id,
                        scope,
                        audit: secret_audit_context(),
                    })
                    .await
                    .map_err(secret_error_to_vm)?;
                if method == "read_bytes" {
                    return Ok(VmValue::Bytes(std::sync::Arc::new(
                        secret.with_exposed(|bytes| bytes.to_vec()),
                    )));
                }
                let text = secret.with_exposed(|bytes| {
                    std::str::from_utf8(bytes)
                        .map(str::to_string)
                        .map_err(|error| {
                            VmError::TypeError(format!(
                                "{}.read secret `{name}` was not UTF-8: {error}",
                                handle.type_name()
                            ))
                        })
                })?;
                Ok(vm_string(text))
            }
            "write" => {
                if args.len() > 4 {
                    return Err(VmError::TypeError(format!(
                        "{}.write expects name, value, optional scope, and optional ttl",
                        handle.type_name()
                    )));
                }
                let name = secret_name_arg(handle, method, args.first())?;
                let value = secret_value_arg(handle, method, args.get(1), "value")?;
                let scope = secret_scope_arg(args.get(2))?;
                let ttl =
                    optional_duration_arg(args.get(3), &format!("{}.write", handle.type_name()))?;
                let id = secret_id_for_scope(&name, &scope)?;
                crate::secrets::ensure_scoped_secret_access_allowed(method, &id)
                    .map_err(secret_error_to_vm)?;
                let receipt = provider
                    .write_scoped(crate::secrets::SecretWriteRequest {
                        id,
                        scope,
                        value,
                        options: crate::secrets::SecretWriteOptions { ttl },
                        audit: secret_audit_context(),
                    })
                    .await
                    .map_err(secret_error_to_vm)?;
                Ok(secret_write_receipt_value(receipt))
            }
            "rotate" => {
                if args.len() > 4 {
                    return Err(VmError::TypeError(format!(
                        "{}.rotate expects name, generator/value, optional scope, and optional options",
                        handle.type_name()
                    )));
                }
                let name = secret_name_arg(handle, method, args.first())?;
                let value = match args.get(1) {
                    Some(VmValue::Closure(closure)) => {
                        let generated = self.call_closure_pub(closure, &[]).await?;
                        secret_value_from_vm(handle, method, &generated, "generator result")?
                    }
                    other => secret_value_arg(handle, method, other, "value")?,
                };
                let scope = secret_scope_arg(args.get(2))?;
                let options = secret_rotation_options_arg(args.get(3))?;
                let id = secret_id_for_scope(&name, &scope)?;
                crate::secrets::ensure_scoped_secret_access_allowed(method, &id)
                    .map_err(secret_error_to_vm)?;
                let receipt = provider
                    .rotate_scoped(crate::secrets::SecretRotateRequest {
                        id,
                        scope,
                        value,
                        options,
                        audit: secret_audit_context(),
                    })
                    .await
                    .map_err(secret_error_to_vm)?;
                Ok(secret_rotation_receipt_value(receipt))
            }
            "delete" => {
                if args.len() > 2 {
                    return Err(VmError::TypeError(format!(
                        "{}.delete expects name and optional scope",
                        handle.type_name()
                    )));
                }
                let name = secret_name_arg(handle, method, args.first())?;
                let scope = secret_scope_arg(args.get(1))?;
                let id = secret_id_for_scope(&name, &scope)?;
                crate::secrets::ensure_scoped_secret_access_allowed(method, &id)
                    .map_err(secret_error_to_vm)?;
                provider
                    .delete_scoped(crate::secrets::SecretDeleteRequest {
                        id,
                        scope,
                        audit: secret_audit_context(),
                    })
                    .await
                    .map_err(secret_error_to_vm)?;
                Ok(VmValue::Nil)
            }
            "lease" | "lease_bytes" => {
                if args.len() > 3 {
                    return Err(VmError::TypeError(format!(
                        "{}.{method} expects name, duration, and optional scope",
                        handle.type_name(),
                    )));
                }
                let name = secret_name_arg(handle, method, args.first())?;
                let duration = required_duration_arg(
                    args.get(1),
                    &format!("{}.{}", handle.type_name(), method),
                )?;
                let scope = secret_scope_arg(args.get(2))?;
                let id = secret_id_for_scope(&name, &scope)?;
                crate::secrets::ensure_scoped_secret_access_allowed(method, &id)
                    .map_err(secret_error_to_vm)?;
                let grant = provider
                    .lease_scoped(crate::secrets::SecretLeaseRequest {
                        id,
                        scope,
                        duration,
                        audit: secret_audit_context(),
                    })
                    .await
                    .map_err(secret_error_to_vm)?;
                Ok(secret_lease_grant_value(method == "lease_bytes", grant)?)
            }
            _ => Err(method_unsupported(handle, method)),
        }
    }

    async fn call_harness_llm_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        let provider = handle.inner().secret_provider().cloned();
        crate::secrets::with_active_secret_provider(
            provider,
            self.call_harness_llm_method_with_session_secrets(handle, method, args),
        )
        .await
    }

    async fn call_harness_llm_method_with_session_secrets(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        match method {
            "call" => self.call_capability_builtin("llm_call", args.to_vec()).await,
            "self_certainty" => {
                self.call_capability_builtin("__llm_self_certainty", args.to_vec())
                    .await
            }
            "call_safe" => {
                self.call_capability_builtin("llm_call_safe", args.to_vec())
                    .await
            }
            "call_structured" => {
                self.call_capability_builtin("llm_call_structured", args.to_vec())
                    .await
            }
            "call_structured_safe" => {
                self.call_capability_builtin("llm_call_structured_safe", args.to_vec())
                    .await
            }
            "call_structured_result" => {
                self.call_capability_builtin("llm_call_structured_result", args.to_vec())
                    .await
            }
            "recover_schema" => {
                self.call_capability_builtin("schema_recover", args.to_vec())
                    .await
            }
            "completion" => {
                self.call_capability_builtin("llm_completion", args.to_vec())
                    .await
            }
            "stream" => self.call_capability_builtin("llm_stream", args.to_vec()).await,
            "with_rate_limit" => {
                self.call_capability_builtin("with_rate_limit", args.to_vec())
                    .await
            }
            "stream_call" => {
                self.call_capability_builtin("llm_stream_call", args.to_vec())
                    .await
            }
            "mock_clear" => {
                self.call_capability_builtin("llm_mock_clear", args.to_vec())
                    .await
            }
            "mock_enqueue" => self.call_capability_builtin("llm_mock", args.to_vec()).await,
            "mock_load_jsonl" => {
                self.call_capability_builtin("llm_mock_load_jsonl", args.to_vec())
                    .await
            }
            "mock_calls" => {
                self.call_capability_builtin("llm_mock_calls", args.to_vec())
                    .await
            }
            "mock_snapshot" => {
                self.call_capability_builtin("llm_mock_snapshot", args.to_vec())
                    .await
            }
            "mock_push_scope" => {
                self.call_capability_builtin("llm_mock_push_scope", args.to_vec())
                    .await
            }
            "mock_pop_scope" => {
                self.call_capability_builtin("llm_mock_pop_scope", args.to_vec())
                    .await
            }
            "upload_file" => {
                self.call_capability_builtin("__files_upload", args.to_vec())
                    .await
            }
            "session_cost" | "budget" | "budget_remaining" => {
                self.call_capability_builtin(&format!("__llm_{method}"), args.to_vec())
                    .await
            }
            "catalog" => Ok(crate::llm::config_builtins::llm_catalog_value()),
            "catalog_refresh" => {
                if args.len() > 1 {
                    return Err(VmError::TypeError(
                        "HarnessLlm.catalog_refresh expects at most one options dict".to_string(),
                    ));
                }
                let options = crate::llm::config_builtins::parse_catalog_refresh_options(
                    args.first(),
                    "HarnessLlm.catalog_refresh",
                )?;
                let report = crate::provider_catalog::refresh_runtime_catalog(options).await;
                let json = serde_json::to_value(report).map_err(|error| {
                    VmError::Runtime(format!(
                        "HarnessLlm.catalog_refresh: serialize result: {error}"
                    ))
                })?;
                Ok(crate::stdlib::json_to_vm_value(&json))
            }
            "providers" => Ok(crate::llm::config_builtins::llm_provider_status_value()),
            _ => Err(method_unsupported(handle, method)),
        }
    }

    fn call_harness_tenant_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        Self::call_harness_tenant_method_sync_fast(method, args)
            .unwrap_or_else(|| Err(method_unsupported(handle, method)))
    }

    async fn call_harness_auth_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        let oauth_builtin = match method {
            "oauth_storage_memory" => Some("__oauth_storage_memory_handle"),
            "oauth_storage_file" => Some("__oauth_storage_file_handle"),
            "oauth_storage_cloud" => Some("__oauth_storage_cloud_handle"),
            "oauth_storage_get" => Some("__oauth_storage_get"),
            "oauth_storage_set" => Some("__oauth_storage_set"),
            "oauth_storage_delete" => Some("__oauth_storage_delete"),
            "oauth_storage_with_refresh_lock" => Some("__oauth_storage_with_refresh_lock"),
            "oauth_registration_store" => Some("__oauth_dynreg_store_handle"),
            "oauth_register_client" => Some("__oauth_dynreg_register"),
            "oauth_registered_client" => Some("__oauth_dynreg_get"),
            "oauth_registered_clients" => Some("__oauth_dynreg_list"),
            _ => None,
        };
        if let Some(builtin) = oauth_builtin {
            return self.call_capability_builtin(builtin, args.to_vec()).await;
        }
        Self::call_harness_auth_method_sync_fast(method, args)
            .unwrap_or_else(|| Err(method_unsupported(handle, method)))
    }

    /// Read-only getters over the ambient authenticated principal (see
    /// [`crate::harness_auth`]). Pure thread-local reads — no host state —
    /// so the whole surface rides the sync-fast path. `subject`/`scheme`
    /// raise a typed [`ErrorCategory::Auth`] error when no principal is
    /// bound (mirroring `harness.tenant.id()`); the `try_*` and `kind`
    /// getters return `nil`, and `scopes`/`has_scope`/`is_authenticated`
    /// degrade to empty/false so an unauthenticated route can branch
    /// without try/catch.
    fn call_harness_auth_method_sync_fast(
        method: &str,
        args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        use crate::harness_auth::{current_auth_principal, MISSING_PRINCIPAL_MESSAGE};

        // `has_scope` is the one arity-1 method; everything else is nullary.
        if method == "has_scope" {
            let scope = match args {
                [VmValue::String(scope)] => scope.to_string(),
                [other] => {
                    return Some(Err(VmError::TypeError(format!(
                        "HarnessAuth.has_scope expects a string scope, got {}",
                        other.type_name()
                    ))));
                }
                _ => {
                    return Some(Err(VmError::TypeError(
                        "HarnessAuth.has_scope expects exactly one string argument".to_string(),
                    )));
                }
            };
            let granted = current_auth_principal()
                .map(|principal| principal.scopes.contains(&scope))
                .unwrap_or(false);
            return Some(Ok(VmValue::Bool(granted)));
        }

        let is_nullary_getter = matches!(
            method,
            "is_authenticated"
                | "scopes"
                | "subject"
                | "try_subject"
                | "scheme"
                | "try_scheme"
                | "kind"
        );
        if !is_nullary_getter {
            return None;
        }
        if !args.is_empty() {
            return Some(Err(VmError::TypeError(format!(
                "HarnessAuth.{method} takes no arguments"
            ))));
        }
        let principal = current_auth_principal();
        let auth_error = || VmError::CategorizedError {
            message: MISSING_PRINCIPAL_MESSAGE.to_string(),
            category: ErrorCategory::Auth,
        };
        match method {
            "is_authenticated" => Some(Ok(VmValue::Bool(principal.is_some()))),
            "scopes" => {
                let scopes = principal
                    .map(|principal| {
                        principal
                            .scopes
                            .iter()
                            .map(|scope| vm_string(scope.clone()))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                Some(Ok(VmValue::List(std::sync::Arc::new(scopes))))
            }
            "subject" => Some(match principal {
                Some(principal) if !principal.subject.is_empty() => {
                    Ok(vm_string(principal.subject.clone()))
                }
                _ => Err(auth_error()),
            }),
            "try_subject" => Some(Ok(principal
                .filter(|principal| !principal.subject.is_empty())
                .map(|principal| vm_string(principal.subject.clone()))
                .unwrap_or(VmValue::Nil))),
            "scheme" => Some(match principal {
                Some(principal) if !principal.scheme.is_empty() => {
                    Ok(vm_string(principal.scheme.clone()))
                }
                _ => Err(auth_error()),
            }),
            "try_scheme" => Some(Ok(principal
                .filter(|principal| !principal.scheme.is_empty())
                .map(|principal| vm_string(principal.scheme.clone()))
                .unwrap_or(VmValue::Nil))),
            "kind" => Some(Ok(principal
                .and_then(|principal| principal.kind.clone())
                .map(vm_string)
                .unwrap_or(VmValue::Nil))),
            _ => None,
        }
    }

    async fn call_harness_obs_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        use crate::stdlib::observability::{
            emit_instrument, end_span_typed, log_typed, start_span_typed, MetricInstrument,
        };

        match method {
            "configure" => {
                self.call_capability_builtin("__obs_configure", args.to_vec())
                    .await
            }
            "auto_backend" => {
                self.call_capability_builtin("__obs_auto_backend", args.to_vec())
                    .await
            }
            "emit" => self.call_capability_builtin("__obs_emit", args.to_vec()).await,
            "events" => self.call_capability_builtin("__obs_events", args.to_vec()).await,
            "events_take" => {
                self.call_capability_builtin("__obs_events_take", args.to_vec())
                    .await
            }
            "reset" => self.call_capability_builtin("__obs_reset", args.to_vec()).await,
            "request_id" => {
                require_no_args(handle, method, args)?;
                Ok(crate::observability::request_id::current_request_id()
                    .map(vm_string)
                    .unwrap_or(VmValue::Nil))
            }
            "start_span" => {
                let name = obs_string_arg(handle, method, args.first(), "name")?;
                let attrs = obs_attrs_arg(handle, method, args.get(1))?;
                start_span_typed(name, attrs)
            }
            "end_span" => {
                let handle_arg = args.first().cloned().unwrap_or(VmValue::Nil);
                end_span_typed(handle_arg);
                Ok(VmValue::Nil)
            }
            "span" => {
                let name = obs_string_arg(handle, method, args.first(), "name")?;
                let attrs = obs_attrs_arg(handle, method, args.get(1))?;
                let callback = args.get(2).cloned().unwrap_or(VmValue::Nil);
                let span_handle = start_span_typed(name, attrs)?;
                // No callback → imperative mode: hand the span handle
                // back so the caller can close it with `end_span` later.
                // The other branches always close before returning so
                // an erroring closure can't leak a span.
                let closure = match callback {
                    VmValue::Nil => return Ok(span_handle),
                    VmValue::Closure(closure) => closure,
                    other => {
                        end_span_typed(span_handle);
                        return Err(VmError::TypeError(format!(
                            "{}.span callback must be a closure or nil, got {}",
                            handle.type_name(),
                            other.type_name()
                        )));
                    }
                };
                let result = self.call_closure_pub(&closure, &[]).await;
                end_span_typed(span_handle);
                result
            }
            "log" => {
                // Argument order matches `std/observability::log`:
                // `(message, level = "info", fields = {})`. Keeping the
                // two surfaces in lockstep means a script that imports
                // either one round-trips identically.
                let message = obs_string_arg(handle, method, args.first(), "message")?;
                let level = obs_optional_string_arg(handle, method, args.get(1), "level")?
                    .unwrap_or_else(|| "info".to_string());
                let fields = obs_attrs_arg(handle, method, args.get(2))?;
                let emitted = log_typed(message, level, fields)?;
                Ok(crate::stdlib::json_to_vm_value(&emitted))
            }
            "counter" | "histogram" | "gauge" => {
                let instrument = match method {
                    "counter" => MetricInstrument::Counter,
                    "histogram" => MetricInstrument::Histogram,
                    "gauge" => MetricInstrument::Gauge,
                    _ => unreachable!("outer match restricts the arm"),
                };
                let name = obs_string_arg(handle, method, args.first(), "name")?;
                let value_arg = args.get(1).cloned().unwrap_or(VmValue::Nil);
                let value_json = obs_number_arg(handle, method, &value_arg)?;
                let attrs = obs_attrs_arg(handle, method, args.get(2))?;
                let emitted = emit_instrument(instrument, name, value_json, attrs)?;
                Ok(crate::stdlib::json_to_vm_value(&emitted))
            }
            _ => Err(method_unsupported(handle, method)),
        }
    }
}
