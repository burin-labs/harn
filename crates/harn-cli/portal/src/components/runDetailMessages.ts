import { defineMessages } from "react-intl"

export const runDetailMessages = defineMessages({
  noRunSelectedTitle: {
    id: "portal.detail.noRunSelectedTitle",
    defaultMessage: "No run selected",
  },
  noRunSelectedCopy: {
    id: "portal.detail.noRunSelectedCopy",
    defaultMessage:
      "Pick a run from the left to inspect its stages, spans, transcript story, and child runs.",
  },
  run: { id: "portal.detail.run", defaultMessage: "Run" },
  modelCalls: { id: "portal.detail.modelCalls", defaultMessage: "Model calls" },
  tokens: { id: "portal.detail.tokens", defaultMessage: "Tokens" },
  childRuns: { id: "portal.detail.childRuns", defaultMessage: "Child runs" },
  started: { id: "portal.detail.started", defaultMessage: "Started" },
  capabilityValidation: {
    id: "portal.detail.capabilityValidation",
    defaultMessage: "Capability and validation",
  },
  capabilityValidationCopy: {
    id: "portal.detail.capabilityValidationCopy",
    defaultMessage:
      "The effective top-level ceiling for this run, plus any saved workflow validation report.",
  },
  replayEval: { id: "portal.detail.replayEval", defaultMessage: "Replay and eval" },
  replayEvalCopy: {
    id: "portal.detail.replayEvalCopy",
    defaultMessage:
      "Saved replay expectations derived from this run so you can turn debugging into a repeatable check.",
  },
  lineageExecution: { id: "portal.detail.lineageExecution", defaultMessage: "Lineage and execution" },
  lineageExecutionCopy: {
    id: "portal.detail.lineageExecutionCopy",
    defaultMessage: "Where this run sits in a larger tree, and which local execution context it used.",
  },
  workflowFlow: { id: "portal.detail.workflowFlow", defaultMessage: "Workflow flow" },
  workflowFlowCopy: {
    id: "portal.detail.workflowFlowCopy",
    defaultMessage: "The path this run took through transitions and checkpoints.",
  },
  actionGraph: { id: "portal.detail.actionGraph", defaultMessage: "Action graph" },
  actionGraphCopy: {
    id: "portal.detail.actionGraphCopy",
    defaultMessage:
      "One derived debugging artifact that rolls planner rounds, worker lineage, verification, and transcript pointers into the same view.",
  },
  runComparison: { id: "portal.detail.runComparison", defaultMessage: "Run comparison" },
  runComparisonCopy: {
    id: "portal.detail.runComparisonCopy",
    defaultMessage: "Compare this run against any other persisted run of the same workflow.",
  },
  traceTimeline: { id: "portal.detail.traceTimeline", defaultMessage: "Trace timeline" },
  traceTimelineCopy: {
    id: "portal.detail.traceTimelineCopy",
    defaultMessage: "A horizontal view of where time went across workflow stages and nested runtime spans.",
  },
  stageSummary: { id: "portal.detail.stageSummary", defaultMessage: "Stage summary" },
  stageSummaryCopy: {
    id: "portal.detail.stageSummaryCopy",
    defaultMessage: "Big-picture workflow progress, retries, and verification output.",
  },
  runtimeActivity: { id: "portal.detail.runtimeActivity", defaultMessage: "Runtime activity" },
  runtimeActivityCopy: {
    id: "portal.detail.runtimeActivityCopy",
    defaultMessage: "Span-derived activity feed ordered by when things happened.",
  },
  producedArtifacts: { id: "portal.detail.producedArtifacts", defaultMessage: "Produced artifacts" },
  producedArtifactsCopy: {
    id: "portal.detail.producedArtifactsCopy",
    defaultMessage: "The durable outputs this run saved for later stages, child runs, or inspection.",
  },
  modelTurns: { id: "portal.detail.modelTurns", defaultMessage: "Model turns" },
  modelTurnsCopy: {
    id: "portal.detail.modelTurnsCopy",
    defaultMessage:
      "Saved request/response turns from llm_transcript.jsonl when a transcript sidecar exists.",
  },
  variantResolution: {
    id: "portal.detail.variantResolution",
    defaultMessage: "Variant resolution",
  },
  variantResolutionCopy: {
    id: "portal.detail.variantResolutionCopy",
    defaultMessage:
      "Which capability-adaptive branches fired in each prompt template, alongside the resolved LLM identity and capability snapshot.",
  },
  noTemplateRenders: {
    id: "portal.detail.noTemplateRenders",
    defaultMessage:
      "No template.render events were captured for this run. (Variant resolution is only recorded when render() runs inside an LLM-aware frame.)",
  },
  variantBranchesNone: {
    id: "portal.detail.variantBranchesNone",
    defaultMessage: "No capability branches recorded.",
  },
  variantCapabilitiesNone: {
    id: "portal.detail.variantCapabilitiesNone",
    defaultMessage: "No capability snapshot recorded.",
  },
  transcriptStory: { id: "portal.detail.transcriptStory", defaultMessage: "Transcript story" },
  transcriptStoryCopy: {
    id: "portal.detail.transcriptStoryCopy",
    defaultMessage: "Human-visible transcript sections from the run and its stages.",
  },
  children: { id: "portal.detail.children", defaultMessage: "Child runs" },
  childrenCopy: {
    id: "portal.detail.childrenCopy",
    defaultMessage: "Delegated work launched under this run.",
  },
  baselineRun: { id: "portal.detail.baselineRun", defaultMessage: "Baseline run" },
  comparisonFailed: { id: "portal.detail.comparisonFailed", defaultMessage: "Comparison failed: {message}" },
  noCompareCandidates: {
    id: "portal.detail.noCompareCandidates",
    defaultMessage: "No other runs of this workflow were found to compare against.",
  },
  noStageDiffs: {
    id: "portal.detail.noStageDiffs",
    defaultMessage: "No stage-level differences were detected.",
  },
  noObservabilityDiffs: {
    id: "portal.detail.noObservabilityDiffs",
    defaultMessage: "No observability differences were detected.",
  },
  noReplayFixture: {
    id: "portal.detail.noReplayFixture",
    defaultMessage: "No replay fixture was saved with this run yet.",
  },
  replayCommand: { id: "portal.detail.replayCommand", defaultMessage: "Replay command" },
  evalCommand: { id: "portal.detail.evalCommand", defaultMessage: "Eval command" },
  stageInternals: { id: "portal.detail.stageInternals", defaultMessage: "Stage internals" },
  openChildRun: { id: "portal.detail.openChildRun", defaultMessage: "Open child run" },
  noToolCalls: { id: "portal.detail.noToolCalls", defaultMessage: "No tool calls recorded." },
  noResponseText: {
    id: "portal.detail.noResponseText",
    defaultMessage: "No response text persisted for this step.",
  },
  noAddedContext: {
    id: "portal.detail.noAddedContext",
    defaultMessage: "No newly added context captured for this step.",
  },
  noTurns: {
    id: "portal.detail.noTurns",
    defaultMessage: "No saved model transcript sidecar found for this run.",
  },
  noStory: {
    id: "portal.detail.noStory",
    defaultMessage: "No human-visible transcript sections were saved for this run.",
  },
  noChildren: {
    id: "portal.detail.noChildren",
    defaultMessage: "No delegated child runs for this run.",
  },
  noPlannerRounds: {
    id: "portal.detail.noPlannerRounds",
    defaultMessage: "No planner-round summaries were persisted for this run.",
  },
  noTranscriptPointers: {
    id: "portal.detail.noTranscriptPointers",
    defaultMessage: "No transcript pointers were available for this run.",
  },
  noWorkerLineage: {
    id: "portal.detail.noWorkerLineage",
    defaultMessage: "No delegated worker lineage was captured for this run.",
  },
  noDaemonEvents: {
    id: "portal.detail.noDaemonEvents",
    defaultMessage: "No daemon lifecycle events were captured for this run.",
  },
  noActivity: {
    id: "portal.detail.noActivity",
    defaultMessage: "No span activity captured for this run.",
  },
  auditLayersTooltip: {
    id: "portal.detail.auditLayersTooltip",
    defaultMessage: "Middleware layers (outer → inner)",
  },
  auditReceiptLink: {
    id: "portal.detail.auditReceiptLink",
    defaultMessage: "View receipt",
  },
  noArtifacts: {
    id: "portal.detail.noArtifacts",
    defaultMessage: "This run did not persist any artifacts.",
  },
  noTransitions: {
    id: "portal.detail.noTransitions",
    defaultMessage: "No workflow transitions were persisted.",
  },
  noCheckpoints: {
    id: "portal.detail.noCheckpoints",
    defaultMessage: "No checkpoints were persisted.",
  },
  validationPassed: { id: "portal.detail.validationPassed", defaultMessage: "Validation passed" },
  validationFailed: { id: "portal.detail.validationFailed", defaultMessage: "Validation failed" },
  noValidationReport: { id: "portal.detail.noValidationReport", defaultMessage: "No validation report" },
  unrestricted: { id: "portal.detail.unrestricted", defaultMessage: "unrestricted" },
  notDeclared: { id: "portal.detail.notDeclared", defaultMessage: "not declared" },
  noExplicitCapabilityOps: {
    id: "portal.detail.noExplicitCapabilityOps",
    defaultMessage: "No explicit capability operations",
  },
  noWorkspaceRoots: {
    id: "portal.detail.noWorkspaceRoots",
    defaultMessage: "No workspace roots persisted",
  },
  noArgConstraints: {
    id: "portal.detail.noArgConstraints",
    defaultMessage: "No tool arg constraints",
  },
  noValidationErrors: {
    id: "portal.detail.noValidationErrors",
    defaultMessage: "No validation errors saved",
  },
  noValidationWarnings: {
    id: "portal.detail.noValidationWarnings",
    defaultMessage: "No validation warnings saved",
  },
  noReachableNodes: {
    id: "portal.detail.noReachableNodes",
    defaultMessage: "No reachable-node report saved",
  },
  workflowStages: { id: "portal.detail.workflowStages", defaultMessage: "Workflow stages" },
  workflowStagesCopy: {
    id: "portal.detail.workflowStagesCopy",
    defaultMessage: "Big-picture progress across the run",
  },
  nestedSpans: { id: "portal.detail.nestedSpans", defaultMessage: "Nested spans" },
  nestedSpansCopy: {
    id: "portal.detail.nestedSpansCopy",
    defaultMessage: "Runtime calls and operations inside those stages",
  },
})
