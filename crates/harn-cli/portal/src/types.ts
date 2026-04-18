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

export type PortalActivity = {
  label: string
  kind: string
  started_offset_ms: number
  duration_ms: number
  stage_node_id: string | null
  call_id: string | null
  summary: string
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
  story: PortalStorySection[]
  child_runs: PortalChildRun[]
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
