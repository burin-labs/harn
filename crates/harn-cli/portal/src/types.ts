export type RunSummary = {
  path: string
  id: string
  workflow_name: string
  status: string
  last_stage_node_id: string | null
  failure_summary: string | null
  started_at: string
  finished_at: string | null
  duration_ms: number | null
  stage_count: number
  child_run_count: number
  call_count: number
  input_tokens: number
  output_tokens: number
  models: string[]
  updated_at_ms: number
  skills: string[]
}

export type PortalStats = {
  total_runs: number
  completed_runs: number
  active_runs: number
  failed_runs: number
  avg_duration_ms: number
}

export type TrustRecord = {
  schema: string
  record_id: string
  agent: string
  action: string
  approver: string | null
  outcome: "success" | "failure" | "denied" | "timeout"
  trace_id: string
  autonomy_tier: "shadow" | "suggest" | "act_with_approval" | "act_auto"
  timestamp: string
  cost_usd: number | null
  chain_index: number
  previous_hash: string | null
  entry_hash: string
  metadata: Record<string, unknown>
}

export type TrustTraceGroup = {
  trace_id: string
  records: TrustRecord[]
}

export type TrustAgentSummary = {
  agent: string
  total: number
  success_rate: number
  mean_cost_usd: number | null
  tier_distribution: Record<string, number>
  outcome_distribution: Record<string, number>
}

export type TrustChainReport = {
  topic: string
  total: number
  verified: boolean
  root_hash: string | null
  broken_at_event_id: number | null
  errors: string[]
}

export type PortalTrustGraphResponse = {
  records: TrustRecord[]
  groups: TrustTraceGroup[] | null
  summary: TrustAgentSummary[]
  chain: TrustChainReport
  topics: string[]
}

export type PortalListResponse = {
  stats: PortalStats
  filtered_count: number
  pagination: PortalPagination
  runs: RunSummary[]
}

export type PortalPagination = {
  page: number
  page_size: number
  total_pages: number
  total_runs: number
  has_previous: boolean
  has_next: boolean
}

export type PortalMeta = {
  workspace_root: string
  run_dir: string
}

export type PersonaManifestSummary = {
  name: string
  version: string | null
  description: string
  entry_workflow: string
  tools: string[]
  capabilities: string[]
  autonomy_tier: string
  receipt_policy: string
  triggers: string[]
  schedules: string[]
  model_policy: Record<string, unknown>
  budget: Record<string, unknown>
  handoffs: string[]
  context_packs: string[]
  evals: string[]
  manifest_path: string
}

export type PersonaLifecycleState =
  | "inactive"
  | "starting"
  | "idle"
  | "running"
  | "paused"
  | "draining"
  | "failed"
  | "disabled"

export type PersonaLease = {
  id: string
  holder: string
  work_key: string
  acquired_at_ms: number
  expires_at_ms: number
}

export type PersonaAssignmentStatus = {
  work_key: string
  lease_id: string
  holder: string
  acquired_at: string
  expires_at: string
}

export type PersonaBudgetStatus = {
  daily_usd: number | null
  hourly_usd: number | null
  run_usd: number | null
  max_tokens: number | null
  spent_today_usd: number
  spent_this_hour_usd: number
  spent_last_run_usd: number
  tokens_today: number
  remaining_today_usd: number | null
  remaining_hour_usd: number | null
  exhausted: boolean
  reason: string | null
  last_receipt_id: string | null
}

export type PersonaQueuedWork = {
  work_key: string
  provider: string
  kind: string
  queued_at: string
  reason: string
  source_event_id: string | null
  metadata: Record<string, string>
}

export type PersonaHandoffInboxItem = {
  work_key: string
  handoff_id: string | null
  handoff_kind: string | null
  source_persona: string | null
  task: string | null
  queued_at: string
  reason: string
}

export type PersonaValueReceipt = {
  kind: string
  run_id: string | null
  occurred_at: string
  paid_cost_usd: number
  avoided_cost_usd: number
  deterministic_steps: number
  llm_steps: number
  metadata: unknown
}

export type PersonaStatus = {
  name: string
  template_ref: string | null
  state: PersonaLifecycleState
  entry_workflow: string
  role: string
  current_assignment: PersonaAssignmentStatus | null
  last_run: string | null
  next_scheduled_run: string | null
  active_lease: PersonaLease | null
  budget: PersonaBudgetStatus
  last_error: string | null
  queued_events: number
  queued_work: PersonaQueuedWork[]
  handoff_inbox: PersonaHandoffInboxItem[]
  value_receipts: PersonaValueReceipt[]
  disabled_events: number
  paused_event_policy: string
}

export type PortalHighlightKeywords = {
  keyword: string[]
  literal: string[]
  built_in: string[]
}

export type PortalLlmProviderOption = {
  name: string
  base_url: string
  base_url_env: string | null
  auth_style: string
  auth_envs: string[]
  auth_configured: boolean
  viable: boolean
  local: boolean
  models: string[]
  aliases: string[]
  default_model: string
}

export type PortalLlmOptions = {
  preferred_provider: string | null
  preferred_model: string | null
  providers: PortalLlmProviderOption[]
}

export type PortalInsight = {
  label: string
  value: string
  detail: string
}

export type PortalCostSummary = {
  total_cost_usd: number
  call_count: number
  input_tokens: number
  output_tokens: number
}

export type PortalCostTrendPoint = {
  date: string
  pipeline: string
  cost_usd: number
  call_count: number
  input_tokens: number
  output_tokens: number
}

export type PortalProviderCostBreakdown = {
  provider: string
  model: string
  cost_usd: number
  call_count: number
  input_tokens: number
  output_tokens: number
}

export type PortalCostReport = {
  summary: PortalCostSummary
  trend: PortalCostTrendPoint[]
  provider_breakdown: PortalProviderCostBreakdown[]
}

export type PortalStageDebug = {
  call_count: number
  input_tokens: number
  output_tokens: number
  consumed_artifact_ids: string[]
  produced_artifact_ids: string[]
  selected_artifact_ids: string[]
  worker_id: string | null
  error: string | null
  model_policy: string | null
  auto_compact: string | null
  output_visibility: string | null
  context_policy: string | null
  retry_policy: string | null
  capability_policy: string | null
  input_contract: string | null
  output_contract: string | null
  prompt: string | null
  system_prompt: string | null
  rendered_context: string | null
}

export type PortalStage = {
  id: string
  node_id: string
  kind: string
  status: string
  outcome: string
  branch: string | null
  started_at: string
  finished_at: string | null
  duration_ms: number | null
  artifact_count: number
  attempt_count: number
  verification_summary: string | null
  debug: PortalStageDebug
}

export type PortalSpan = {
  span_id: number
  parent_id: number | null
  kind: string
  name: string
  start_ms: number
  duration_ms: number
  end_ms: number
  label: string
  lane: number
  depth: number
  metadata: Record<string, unknown>
}

export type PortalActivityAuditLayer = {
  name: string
  status: string
  metadata?: Record<string, unknown>
}

export type PortalActivityAudit = {
  reason?: string
  kind?: string
  status: string
  layers: PortalActivityAuditLayer[]
  receipt_uri?: string
}

export type PortalActivity = {
  label: string
  kind: string
  started_offset_ms: number
  duration_ms: number
  stage_node_id: string | null
  call_id: string | null
  summary: string
  audit?: PortalActivityAudit
}

export type PortalTransition = {
  from_node_id: string | null
  to_node_id: string
  branch: string | null
  consumed_count: number
  produced_count: number
}

export type PortalCheckpoint = {
  reason: string
  ready_count: number
  completed_count: number
  last_stage_id: string | null
}

export type PortalArtifact = {
  id: string
  kind: string
  title: string
  source: string | null
  stage: string | null
  estimated_tokens: number | null
  lineage_count: number
  preview: string
}

export type PortalPolicySummary = {
  tools: string[]
  capabilities: string[]
  workspace_roots: string[]
  side_effect_level: string | null
  recursion_limit: number | null
  tool_arg_constraints: string[]
  validation_valid: boolean | null
  validation_errors: string[]
  validation_warnings: string[]
  reachable_nodes: string[]
}

export type PortalReplayAssertion = {
  node_id: string
  expected_status: string
  expected_outcome: string
  expected_branch: string | null
  required_artifact_kinds: string[]
  visible_text_contains: string | null
}

export type PortalReplaySummary = {
  fixture_id: string
  source_run_id: string
  created_at: string
  expected_status: string
  stage_assertions: PortalReplayAssertion[]
}

export type PortalTranscriptMessage = {
  role: string
  content: string
}

export type PortalTranscriptStep = {
  call_id: string
  span_id: number | null
  iteration: number
  call_index: number
  model: string
  provider: string | null
  kept_messages: number
  added_messages: number
  total_messages: number
  input_tokens: number | null
  output_tokens: number | null
  system_prompt: string | null
  added_context: PortalTranscriptMessage[]
  response_text: string | null
  thinking: string | null
  tool_calls: string[]
  summary: string
}

export type PortalTemplateBranch = {
  kind: string
  template_uri: string
  line: number
  col: number
  branch_id: string
  branch_label: string | null
}

export type PortalTemplateRender = {
  template_uri: string
  template_revision_hash: string
  rendered_bytes: number
  provider: string
  model: string
  family: string
  capabilities: Record<string, unknown>
  branches: PortalTemplateBranch[]
  span_id: number | null
  timestamp: string | null
}

export type PortalStorySection = {
  title: string
  scope: string
  role: string
  source: string
  text: string
  preview: string
}

export type PortalChildRun = {
  worker_name: string
  status: string
  started_at: string
  finished_at: string | null
  run_id: string | null
  run_path: string | null
  task: string
}

export type RunDeliverableSummary = {
  id: string
  text: string
  status: string
  note: string | null
}

export type RunTaskLedgerSummary = {
  root_task: string
  rationale: string
  deliverables: RunDeliverableSummary[]
  observations: string[]
  blocking_count: number
}

export type RunPlannerRound = {
  stage_id: string
  node_id: string
  stage_kind: string
  status: string
  outcome: string
  iteration_count: number
  llm_call_count: number
  tool_execution_count: number
  tool_rejection_count: number
  intervention_count: number
  compaction_count: number
  tools_used: string[]
  successful_tools: string[]
  ledger_done_rejections: number
  task_ledger: RunTaskLedgerSummary | null
  research_facts: string[]
}

export type RunWorkerLineage = {
  worker_id: string
  worker_name: string
  parent_stage_id: string | null
  task: string
  status: string
  session_id: string | null
  parent_session_id: string | null
  run_id: string | null
  run_path: string | null
  snapshot_path: string | null
}

export type RunActionGraphNode = {
  id: string
  label: string
  kind: string
  status: string
  outcome: string
  trace_id: string | null
  stage_id: string | null
  node_id: string | null
  worker_id: string | null
  run_id: string | null
  run_path: string | null
  metadata: Record<string, unknown>
}

export type RunActionGraphEdge = {
  from_id: string
  to_id: string
  kind: string
  label: string | null
}

export type RunVerificationOutcome = {
  stage_id: string
  node_id: string
  status: string
  passed: boolean | null
  summary: string | null
}

export type RunTranscriptPointer = {
  id: string
  label: string
  kind: string
  location: string
  path: string | null
  available: boolean
}

export type DaemonEvent = {
  daemon_id: string
  name: string
  kind: "spawned" | "triggered" | "snapshotted" | "resumed" | "stopped"
  timestamp: string
  persist_path: string
  payload_summary: string | null
}

export type RunObservability = {
  schema_version: number
  planner_rounds: RunPlannerRound[]
  research_fact_count: number
  action_graph_nodes: RunActionGraphNode[]
  action_graph_edges: RunActionGraphEdge[]
  worker_lineage: RunWorkerLineage[]
  verification_outcomes: RunVerificationOutcome[]
  transcript_pointers: RunTranscriptPointer[]
  daemon_events: DaemonEvent[]
}

export type PortalExecutionSummary = {
  cwd: string | null
  repo_path: string | null
  worktree_path: string | null
  branch: string | null
  adapter: string | null
}

export type PortalSkillTimelineEntry = {
  name: string
  description: string
  activated_iteration: number
  deactivated_iteration: number | null
  score: number | null
  reason: string
  allowed_tools: string[]
  scope: string
}

export type PortalSkillMatchCandidate = {
  name: string
  score: number
  reason: string
  activated: boolean
}

export type PortalSkillMatchEvent = {
  iteration: number
  strategy: string
  reassess: boolean
  working_files: string[]
  candidates: PortalSkillMatchCandidate[]
}

export type PortalToolLoadEvent = {
  query: string
  strategy: string
  mode: string
  tool_use_id: string | null
  promoted: string[]
  references: string[]
  iteration: number | null
  scope: string
}

export type PortalRunDetail = {
  summary: RunSummary
  task: string
  workflow_id: string
  parent_run_id: string | null
  root_run_id: string | null
  policy_summary: PortalPolicySummary
  replay_summary: PortalReplaySummary | null
  execution: unknown
  insights: PortalInsight[]
  stages: PortalStage[]
  spans: PortalSpan[]
  activities: PortalActivity[]
  transitions: PortalTransition[]
  checkpoints: PortalCheckpoint[]
  artifacts: PortalArtifact[]
  execution_summary: PortalExecutionSummary | null
  transcript_steps: PortalTranscriptStep[]
  template_renders: PortalTemplateRender[]
  story: PortalStorySection[]
  child_runs: PortalChildRun[]
  observability: RunObservability
  skill_timeline: PortalSkillTimelineEntry[]
  skill_match_events: PortalSkillMatchEvent[]
  tool_load_events: PortalToolLoadEvent[]
  active_skills: string[]
}

export type PortalRunDiff = {
  left_path: string
  right_path: string
  identical: boolean
  status_changed: boolean
  left_status: string
  right_status: string
  stage_diffs: Array<{
    node_id: string
    change: string
    details: string[]
  }>
  tool_diffs: Array<{
    tool_name: string
    args_hash: string
    result_changed: boolean
    left_result: string | null
    right_result: string | null
  }>
  observability_diffs: Array<{
    section: string
    label: string
    details: string[]
  }>
  transition_count_delta: number
  artifact_count_delta: number
  checkpoint_count_delta: number
}

export type PortalLaunchTarget = {
  path: string
  group: string
}

export type PortalLaunchTargetList = {
  targets: PortalLaunchTarget[]
}

export type PortalLaunchJob = {
  id: string
  mode: string
  target_label: string
  status: string
  started_at: string
  finished_at: string | null
  exit_code: number | null
  logs: string
  discovered_run_paths: string[]
  workspace_dir: string | null
  transcript_path: string | null
}

export type PortalLaunchJobList = {
  jobs: PortalLaunchJob[]
}

export type PortalDlqAttempt = {
  attempt: number
  at: string
  status: string
  error: string | null
}

export type PortalDlqEntry = {
  id: string
  event_id: string
  trigger_id: string
  binding_id: string
  binding_key: string
  binding_version: number | null
  provider: string
  event_kind: string
  failed_at: string
  failed_at_ms: number
  last_error: string
  error_class: string
  retry_count: number
  state: string
  headers: Record<string, string>
  payload: unknown
  event: unknown
  attempt_history: PortalDlqAttempt[]
  predicate_trace: unknown[]
}

export type PortalDlqGroup = {
  error_class: string
  count: number
  newest_failed_at: string | null
}

export type PortalDlqAlert = {
  trigger_id: string
  error_class: string
  count: number
  window_seconds: number
  threshold_entries: number
  destinations: string[]
}

export type PortalDlqAlertConfig = {
  trigger_id: string
  destinations: string[]
  threshold_entries: number | null
  threshold_percent: number | null
}

export type PortalDlqListResponse = {
  total: number
  entries: PortalDlqEntry[]
  groups: PortalDlqGroup[]
  alerts: PortalDlqAlert[]
  alert_configs: PortalDlqAlertConfig[]
}

export type PortalDlqBulkResponse = {
  operation: string
  dry_run: boolean
  matched_count: number
  accepted_count: number
  skipped_count: number
  rate_limit_per_second: number
  jobs: PortalLaunchJob[]
  entries: PortalDlqEntry[]
}
