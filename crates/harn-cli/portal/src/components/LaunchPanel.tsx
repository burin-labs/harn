import { lazy, Suspense, useEffect, useState } from "react"
import { defineMessages, useIntl } from "react-intl"

import type { PortalLaunchJob, PortalLaunchTarget, PortalLlmOptions, PortalMeta } from "../types"

const CodeEditor = lazy(async () => {
  const module = await import("./CodeEditor")
  return { default: module.CodeEditor }
})

const HELLO_WORLD_SOURCE = `pipeline main() {
  println("hello from portal")
}
`

const WORKFLOW_SOURCE = `pipeline main() {
  let draft = artifact({kind: "summary", text: "Seed context from the portal", relevance: 0.8})
  let flow = workflow_graph({
    name: "portal_workflow",
    entry: "act",
    nodes: {
      act: {
        kind: "stage",
        mode: "llm",
        output_contract: {output_kinds: ["summary"]},
      },
    },
    edges: [],
  })
  let result = workflow_execute(
    "Summarize the current repository in a few bullets.",
    flow,
    [draft],
    {max_steps: 4},
  )
  println(result?.status)
}
`

const messages = defineMessages({
  title: { id: "portal.launch.title", defaultMessage: "Launch" },
  copy: {
    id: "portal.launch.copy",
    defaultMessage:
      "Run Harn directly from the portal. Launches use the portal server's workspace root and persist outputs into the watched run directory.",
  },
  mode: { id: "portal.launch.mode", defaultMessage: "Mode" },
  fileMode: { id: "portal.launch.fileMode", defaultMessage: "Existing file" },
  sourceMode: { id: "portal.launch.sourceMode", defaultMessage: "Script editor" },
  playgroundMode: { id: "portal.launch.playgroundMode", defaultMessage: "Playground" },
  target: { id: "portal.launch.target", defaultMessage: "Target file" },
  source: { id: "portal.launch.source", defaultMessage: "Harn source" },
  task: { id: "portal.launch.task", defaultMessage: "Task" },
  provider: { id: "portal.launch.provider", defaultMessage: "Provider" },
  model: { id: "portal.launch.model", defaultMessage: "Model" },
  customProvider: { id: "portal.launch.customProvider", defaultMessage: "Custom provider" },
  customModel: { id: "portal.launch.customModel", defaultMessage: "Custom model" },
  env: { id: "portal.launch.env", defaultMessage: "Env JSON overrides" },
  run: { id: "portal.launch.run", defaultMessage: "Run now" },
  running: { id: "portal.launch.running", defaultMessage: "Launching…" },
  taskHint: {
    id: "portal.launch.taskHint",
    defaultMessage: "Write a concise report about the repository status.",
  },
  jobs: { id: "portal.launch.jobs", defaultMessage: "Recent launches" },
  noJobs: { id: "portal.launch.noJobs", defaultMessage: "No launch jobs yet." },
  openRun: { id: "portal.launch.openRun", defaultMessage: "Open run" },
  workspaceDir: { id: "portal.launch.workspaceDir", defaultMessage: "Workspace" },
  transcriptPath: { id: "portal.launch.transcriptPath", defaultMessage: "Transcript" },
  logs: { id: "portal.launch.logs", defaultMessage: "Logs" },
  workspaceRoot: { id: "portal.launch.workspaceRoot", defaultMessage: "Workspace root" },
  runDir: { id: "portal.launch.runDir", defaultMessage: "Run artifacts" },
  launchContext: { id: "portal.launch.launchContext", defaultMessage: "Launch context" },
  launchContextCopy: {
    id: "portal.launch.launchContextCopy",
    defaultMessage: "Existing-file launches run from this workspace. Playground launches also persist generated source and metadata under the run directory.",
  },
  sourceHelp: {
    id: "portal.launch.sourceHelp",
    defaultMessage: "Write and execute a Harn script directly in the portal. The script runs with the workspace root shown above as its current directory.",
  },
  playgroundHelp: {
    id: "portal.launch.playgroundHelp",
    defaultMessage: "Create a quick persisted workflow run from a task and optional provider/model overrides.",
  },
  fileHelp: {
    id: "portal.launch.fileHelp",
    defaultMessage: "Run an existing .harn file from the current workspace.",
  },
  presetHello: { id: "portal.launch.presetHello", defaultMessage: "Hello world" },
  presetWorkflow: { id: "portal.launch.presetWorkflow", defaultMessage: "Workflow graph" },
  editorPresets: { id: "portal.launch.editorPresets", defaultMessage: "Templates" },
  sectionEditor: { id: "portal.launch.sectionEditor", defaultMessage: "Editor" },
  sectionRuns: { id: "portal.launch.sectionRuns", defaultMessage: "Launch history" },
  selectMode: { id: "portal.launch.selectMode", defaultMessage: "Choose a launch flow" },
  providerHelp: {
    id: "portal.launch.providerHelp",
    defaultMessage: "Configured providers are read from Harn's runtime config. Local providers attempt live model discovery from localhost endpoints.",
  },
  endpointUrl: { id: "portal.launch.endpointUrl", defaultMessage: "Endpoint URL" },
  endpointHelp: {
    id: "portal.launch.endpointHelp",
    defaultMessage: "Use this to point the selected provider at a localhost or LAN-hosted model server without editing config files first.",
  },
  providerUnavailable: {
    id: "portal.launch.providerUnavailable",
    defaultMessage: "{name} (missing auth)",
  },
  customOption: { id: "portal.launch.customOption", defaultMessage: "Custom…" },
  envHelp: {
    id: "portal.launch.envHelp",
    defaultMessage: "JSON object merged into the child process env only for this launch.",
  },
})

type LaunchPanelProps = {
  meta: PortalMeta | null
  llmOptions: PortalLlmOptions | null
  targets: PortalLaunchTarget[]
  jobs: PortalLaunchJob[]
  onLaunch: (payload: {
    file_path?: string
    source?: string
    task?: string
    provider?: string
    model?: string
    env?: Record<string, string>
  }) => Promise<void>
  onOpenRun: (path: string) => void
}

export function LaunchPanel({ meta, llmOptions, targets, jobs, onLaunch, onOpenRun }: LaunchPanelProps) {
  const intl = useIntl()
  const [mode, setMode] = useState<"file" | "source" | "playground">("playground")
  const [filePath, setFilePath] = useState("examples/portal-demo.harn")
  const [source, setSource] = useState(HELLO_WORLD_SOURCE)
  const [task, setTask] = useState("Summarize the repository in a few bullets.")
  const [provider, setProvider] = useState(llmOptions?.preferred_provider ?? "")
  const [model, setModel] = useState(llmOptions?.preferred_model ?? "")
  const [endpointUrl, setEndpointUrl] = useState("")
  const [envJson, setEnvJson] = useState("")
  const [customProviderMode, setCustomProviderMode] = useState(false)
  const [customModelMode, setCustomModelMode] = useState(false)
  const [submitting, setSubmitting] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const selectedProvider =
    llmOptions?.providers.find((item) => item.name === provider) ??
    llmOptions?.providers.find((item) => item.name === llmOptions.preferred_provider) ??
    llmOptions?.providers.find((item) => item.local && item.viable) ??
    llmOptions?.providers.find((item) => item.viable) ??
    null
  const providerValue = provider || selectedProvider?.name || ""
  const selectedModels = selectedProvider?.models ?? []
  const modelValue = model || selectedProvider?.default_model || ""
  const needsCustomModel =
    customModelMode || (modelValue !== "" && selectedModels.length > 0 && !selectedModels.includes(modelValue))

  useEffect(() => {
    if (!llmOptions) {return}
    if (!provider && llmOptions.preferred_provider) {
      setProvider(llmOptions.preferred_provider)
    } else if (!provider) {
      const fallbackProvider =
        llmOptions.providers.find((item) => item.local && item.viable) ??
        llmOptions.providers.find((item) => item.viable) ??
        null
      if (fallbackProvider) {
        setProvider(fallbackProvider.name)
      }
    }
    if (!model && llmOptions.preferred_model) {
      setModel(llmOptions.preferred_model)
    }
  }, [llmOptions, model, provider])

  useEffect(() => {
    if (!selectedProvider) {return}
    if (!endpointUrl) {
      setEndpointUrl(selectedProvider.base_url)
    }
  }, [endpointUrl, selectedProvider])

  async function handleLaunch() {
    setSubmitting(true)
    setError(null)
    try {
      const env = envJson.trim() ? (JSON.parse(envJson) as Record<string, string>) : {}
      if (typeof env !== "object" || Array.isArray(env)) {
        throw new Error("Env JSON must be an object")
      }
      if (selectedProvider?.base_url_env && endpointUrl.trim()) {
        env[selectedProvider.base_url_env] = endpointUrl.trim()
      }
      await onLaunch({
        file_path: mode === "file" ? filePath : undefined,
        source: mode === "source" ? source : undefined,
        task: mode === "playground" ? task : undefined,
        provider: providerValue || undefined,
        model: modelValue || undefined,
        env: Object.keys(env).length ? env : undefined,
      })
    } catch (error) {
      setError(error instanceof Error ? error.message : String(error))
    } finally {
      setSubmitting(false)
    }
  }

  const helperCopy =
    mode === "file"
      ? intl.formatMessage(messages.fileHelp)
      : mode === "source"
        ? intl.formatMessage(messages.sourceHelp)
        : intl.formatMessage(messages.playgroundHelp)

  return (
    <section className="launch-shell">
      <section className="panel launch-panel">
        <div className="panel-header panel-header-spacious">
          <div>
            <div className="eyebrow">{intl.formatMessage(messages.selectMode)}</div>
            <h2>{intl.formatMessage(messages.title)}</h2>
            <p>{intl.formatMessage(messages.copy)}</p>
          </div>
        </div>

        <section className="launch-context">
          <div className="panel-subheader">
            <div>
              <h3>{intl.formatMessage(messages.launchContext)}</h3>
              <p>{intl.formatMessage(messages.launchContextCopy)}</p>
            </div>
          </div>
          <div className="context-grid">
            <div className="context-card">
              <div className="eyebrow">{intl.formatMessage(messages.workspaceRoot)}</div>
              <code>{meta?.workspace_root ?? "Loading…"}</code>
            </div>
            <div className="context-card">
              <div className="eyebrow">{intl.formatMessage(messages.runDir)}</div>
              <code>{meta?.run_dir ?? "Loading…"}</code>
            </div>
          </div>
        </section>

        <div className="mode-switch" role="tablist" aria-label={intl.formatMessage(messages.mode)}>
          {[
            ["playground", intl.formatMessage(messages.playgroundMode)],
            ["source", intl.formatMessage(messages.sourceMode)],
            ["file", intl.formatMessage(messages.fileMode)],
          ].map(([value, label]) => (
            <button
              key={value}
              className={`mode-tab ${mode === value ? "active" : ""}`}
              onClick={() => setMode(value as typeof mode)}
              role="tab"
              aria-selected={mode === value}
              type="button"
            >
              {label}
            </button>
          ))}
        </div>

        <div className="launch-workbench">
          <div className="panel-subheader">
            <div>
              <div className="eyebrow">{intl.formatMessage(messages.sectionEditor)}</div>
              <h3>
                {mode === "source"
                  ? intl.formatMessage(messages.sourceMode)
                  : mode === "file"
                    ? intl.formatMessage(messages.fileMode)
                    : intl.formatMessage(messages.playgroundMode)}
              </h3>
              <p>{helperCopy}</p>
            </div>
          </div>

          {mode === "file" ? (
            <label className="search">
              <span>{intl.formatMessage(messages.target)}</span>
              <select className="compare-select" value={filePath} onChange={(event) => setFilePath(event.target.value)}>
                {targets.map((target) => (
                  <option key={target.path} value={target.path}>
                    {target.group} • {target.path}
                  </option>
                ))}
              </select>
            </label>
          ) : null}

          {mode === "source" ? (
            <div className="editor-block">
              <div className="editor-toolbar">
                <span className="eyebrow">{intl.formatMessage(messages.editorPresets)}</span>
                <div className="preset-row">
                  <button className="ghost-button" onClick={() => setSource(HELLO_WORLD_SOURCE)} type="button">
                    {intl.formatMessage(messages.presetHello)}
                  </button>
                  <button className="ghost-button" onClick={() => setSource(WORKFLOW_SOURCE)} type="button">
                    {intl.formatMessage(messages.presetWorkflow)}
                  </button>
                </div>
              </div>
              <Suspense
                fallback={
                  <textarea
                    className="launch-textarea launch-taskarea"
                    value={source}
                    onChange={(event) => setSource(event.target.value)}
                  />
                }
              >
                <CodeEditor value={source} onChange={setSource} minHeight="320px" />
              </Suspense>
            </div>
          ) : null}

          {mode === "playground" ? (
            <label className="search">
              <span>{intl.formatMessage(messages.task)}</span>
              <textarea
                className="launch-textarea launch-taskarea"
                value={task}
                onChange={(event) => setTask(event.target.value)}
                placeholder={intl.formatMessage(messages.taskHint)}
              />
            </label>
          ) : null}

          <div className="launch-grid">
            <label className="search">
              <span>{intl.formatMessage(messages.provider)}</span>
              <select
                className="compare-select"
                value={customProviderMode ? "__custom__" : providerValue && llmOptions?.providers.some((item) => item.name === providerValue) ? providerValue : "__custom__"}
                onChange={(event) => {
                  if (event.target.value === "__custom__") {
                    setCustomProviderMode(true)
                    setProvider("")
                    return
                  }
                  setCustomProviderMode(false)
                  setProvider(event.target.value)
                  const nextProvider = llmOptions?.providers.find((item) => item.name === event.target.value)
                  setEndpointUrl(nextProvider?.base_url ?? "")
                  setModel(nextProvider?.default_model ?? nextProvider?.models[0] ?? "")
                  setCustomModelMode(false)
                }}
              >
                {llmOptions?.providers.map((item) => (
                  <option key={item.name} value={item.name} disabled={!item.viable}>
                    {item.viable
                      ? item.name
                      : intl.formatMessage(messages.providerUnavailable, { name: item.name })}
                  </option>
                ))}
                <option value="__custom__">{intl.formatMessage(messages.customOption)}</option>
              </select>
            </label>
            {customProviderMode || !providerValue || !llmOptions?.providers.some((item) => item.name === providerValue) ? (
              <label className="search">
                <span>{intl.formatMessage(messages.customProvider)}</span>
                <input value={provider} onChange={(event) => setProvider(event.target.value)} placeholder="local" />
              </label>
            ) : null}
            {selectedProvider?.base_url_env ? (
              <label className="search">
                <span>
                  {intl.formatMessage(messages.endpointUrl)} <span className="muted">({selectedProvider.base_url_env})</span>
                </span>
                <input
                  value={endpointUrl}
                  onChange={(event) => setEndpointUrl(event.target.value)}
                  placeholder={selectedProvider.base_url}
                />
              </label>
            ) : null}
            <label className="search">
              <span>{intl.formatMessage(messages.model)}</span>
              <select
                className="compare-select"
                value={needsCustomModel ? "__custom__" : modelValue}
                onChange={(event) => {
                  if (event.target.value === "__custom__") {
                    setCustomModelMode(true)
                    return
                  }
                  setCustomModelMode(false)
                  setModel(event.target.value)
                }}
              >
                {selectedModels.length ? (
                  selectedModels.map((item) => (
                    <option key={item} value={item}>
                      {item}
                    </option>
                  ))
                ) : (
                  <option value="">{selectedProvider ? "No discovered models" : "Select a provider"}</option>
                )}
                <option value="__custom__">{intl.formatMessage(messages.customOption)}</option>
              </select>
            </label>
            {needsCustomModel || selectedModels.length === 0 ? (
              <label className="search">
                <span>{intl.formatMessage(messages.customModel)}</span>
                <input value={model} onChange={(event) => setModel(event.target.value)} placeholder="qwen2.5-coder:latest" />
              </label>
            ) : null}
            <label className="search launch-full">
              <span>{intl.formatMessage(messages.env)}</span>
              <textarea
                className="launch-textarea launch-env"
                value={envJson}
                onChange={(event) => setEnvJson(event.target.value)}
                placeholder={`{\n  "OPENAI_API_KEY": "...",\n  "ANTHROPIC_API_KEY": "...",\n  "LOCAL_LLM_BASE_URL": "http://192.168.1.40:8000",\n  "LOCAL_LLM_MODEL": "qwen2.5-coder:latest"\n}`}
              />
            </label>
            <div className="muted launch-full">{intl.formatMessage(messages.providerHelp)}</div>
            {selectedProvider?.base_url_env ? <div className="muted launch-full">{intl.formatMessage(messages.endpointHelp)}</div> : null}
            <div className="muted launch-full">{intl.formatMessage(messages.envHelp)}</div>
          </div>

          <div className="launch-actions">
            <button
              className="action-button action-button-primary action-button-inline"
              disabled={submitting || (mode === "file" && !filePath)}
              onClick={() => void handleLaunch()}
              type="button"
            >
              {submitting ? intl.formatMessage(messages.running) : intl.formatMessage(messages.run)}
            </button>
            {error ? <div className="muted">{error}</div> : null}
          </div>
        </div>
      </section>

      <section className="panel launch-history-panel">
        <div className="panel-subheader">
          <div>
            <div className="eyebrow">{intl.formatMessage(messages.sectionRuns)}</div>
            <h3>{intl.formatMessage(messages.jobs)}</h3>
          </div>
        </div>
        <div className="table-like">
          {jobs.length ? (
            jobs.slice(0, 6).map((job) => (
              <div className="artifact-item" key={job.id}>
                <div className="row">
                  <strong>{job.target_label}</strong>
                  <span className={`pill ${job.status === "failed" ? "failed" : job.status === "completed" ? "complete" : "running"}`}>
                    {job.status}
                  </span>
                </div>
                <div className="meta">
                  {job.mode} • started {job.started_at}
                  {job.exit_code != null ? ` • exit ${job.exit_code}` : ""}
                </div>
                {job.workspace_dir ? (
                  <div className="meta">
                    {intl.formatMessage(messages.workspaceDir)} • <code>{job.workspace_dir}</code>
                  </div>
                ) : null}
                {job.transcript_path ? (
                  <div className="meta">
                    {intl.formatMessage(messages.transcriptPath)} • <code>{job.transcript_path}</code>
                  </div>
                ) : null}
                {job.discovered_run_paths.length ? (
                  <div className="turn-chip-row">
                    {job.discovered_run_paths.map((path) => (
                      <button className="run-card-link" key={path} onClick={() => onOpenRun(path)} type="button">
                        {intl.formatMessage(messages.openRun)} {path}
                      </button>
                    ))}
                  </div>
                ) : null}
                {job.logs ? (
                  <details>
                    <summary>{intl.formatMessage(messages.logs)}</summary>
                    <pre>{job.logs}</pre>
                  </details>
                ) : null}
              </div>
            ))
          ) : (
            <div className="muted">{intl.formatMessage(messages.noJobs)}</div>
          )}
        </div>
      </section>
    </section>
  )
}
