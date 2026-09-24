pub(crate) const CASES: [&str; 12] = [
    "eval_prompt_dispatch::terminal_output_is_byte_identical_across_runs",
    "models_dispatch::batch_execution::rejoin_cli_quarantines_an_artifact_without_matching_receipts",
    "models_dispatch::core::models_recommend_human_text_has_model_and_rationale",
    "models_dispatch::lora_inspect_plan::models_lora_inspect_human_text_includes_launch_hint",
    "providers_dispatch::provider_tool_scorecard_human_reports_catalog_mismatch_codes",
    "time_cli::time_run_setup_error_does_not_claim_a_lazy_module_load",
    "trace_import_dispatch::converts_generic_trace_jsonl_to_cli_fixture",
    "predicate_contract::predicate_helper_manifest_survives_warm_cache_and_tracks_changed_question",
    "predicate_contract::predicate_checker_refuses_boolean_use_after_imported_helper",
    "predicate_contract::predicate_census_refuses_an_invalid_imported_site",
    "predicate_contract::predicate_embedding_model_is_refused_at_check_time",
    "predicate_contract::predicate_operation_admission_invalidates_cached_success",
];
