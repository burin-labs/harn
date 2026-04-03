const state = {
  runs: [],
  filteredRuns: [],
  selectedPath: null,
  detail: null,
  compare: null,
  poll: null,
}

const el = (id) => document.getElementById(id)

async function fetchJson(url) {
  const res = await fetch(url)
  if (!res.ok) throw new Error(`Request failed: ${res.status}`)
  return res.json()
}

function formatDuration(ms) {
  if (ms == null) return "n/a"
  if (ms >= 60000) return `${(ms / 60000).toFixed(1)}m`
  if (ms >= 1000) return `${(ms / 1000).toFixed(1)}s`
  return `${ms}ms`
}

function formatNumber(n) {
  if (n == null) return "0"
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`
  return String(n)
}

function statusClass(status) {
  if (status === "complete" || status === "completed" || status === "success" || status === "verified") return "complete"
  if (status === "failed" || status === "error" || status === "cancelled") return status
  return "running"
}

function renderStats(stats) {
  el("stats").innerHTML = [
    ["Runs", stats.total_runs],
    ["Complete", stats.completed_runs],
    ["Active", stats.active_runs],
    ["Failed", stats.failed_runs],
    ["Avg run", formatDuration(stats.avg_duration_ms)],
  ].map(([label, value]) => `
    <div class="card">
      <div class="eyebrow">${label}</div>
      <div class="value">${value}</div>
    </div>
  `).join("")
}

function renderRuns() {
  const list = el("run-list")
  list.innerHTML = state.filteredRuns.map((run) => `
    <button class="run-card ${state.selectedPath === run.path ? "active" : ""}" data-path="${encodeURIComponent(run.path)}">
      <div class="row">
        <strong>${escapeHtml(run.workflow_name)}</strong>
        <span class="pill ${statusClass(run.status)}">${escapeHtml(run.status)}</span>
      </div>
      <div class="meta">${escapeHtml(run.path)}</div>
      <div class="meta">${formatDuration(run.duration_ms)} • ${run.stage_count} stages • ${run.child_run_count} child runs</div>
      <div class="meta">${formatNumber(run.call_count)} calls • ${formatNumber(run.input_tokens + run.output_tokens)} tokens</div>
    </button>
  `).join("")

  list.querySelectorAll("[data-path]").forEach((node) => {
    node.addEventListener("click", () => {
      const path = decodeURIComponent(node.dataset.path)
      selectRun(path)
    })
  })
}

function renderInsights(insights) {
  el("insights").innerHTML = insights.map((item) => `
    <div class="card">
      <div class="eyebrow">${escapeHtml(item.label)}</div>
      <div class="value">${escapeHtml(item.value)}</div>
      <div class="muted">${escapeHtml(item.detail)}</div>
    </div>
  `).join("")
}

function findBaselineRun(current) {
  return state.runs.find((run) =>
    run.path !== current.path &&
    run.workflow_name === current.workflow_name &&
    run.started_at <= current.started_at
  ) || null
}

async function renderCompare(detail) {
  const baseline = findBaselineRun(detail.summary)
  if (!baseline) {
    el("compare").innerHTML = `<div class="muted">No earlier run of this workflow was found to compare against.</div>`
    state.compare = null
    return
  }
  const cacheKey = `${baseline.path}::${detail.summary.path}`
  let diff = state.compare && state.compare.cacheKey === cacheKey ? state.compare.data : null
  if (!diff) {
    diff = await fetchJson(`/api/compare?left=${encodeURIComponent(baseline.path)}&right=${encodeURIComponent(detail.summary.path)}`)
    state.compare = { cacheKey, data: diff }
  }
  el("compare").innerHTML = `
    <div class="compare-summary">
      <span class="turn-chip">baseline ${escapeHtml(baseline.path)}</span>
      <span class="turn-chip">current ${escapeHtml(detail.summary.path)}</span>
      <span class="turn-chip">${diff.identical ? "identical" : "changed"}</span>
      <span class="turn-chip">${escapeHtml(diff.left_status)} → ${escapeHtml(diff.right_status)}</span>
      <span class="turn-chip">${diff.transition_count_delta >= 0 ? "+" : ""}${diff.transition_count_delta} transitions</span>
      <span class="turn-chip">${diff.artifact_count_delta >= 0 ? "+" : ""}${diff.artifact_count_delta} artifacts</span>
      <span class="turn-chip">${diff.checkpoint_count_delta >= 0 ? "+" : ""}${diff.checkpoint_count_delta} checkpoints</span>
    </div>
    ${diff.stage_diffs.length ? diff.stage_diffs.map((item) => `
      <div class="compare-row">
        <div>
          <strong>${escapeHtml(item.node_id)}</strong>
          <div class="meta">${escapeHtml(item.change)}</div>
        </div>
        <div class="meta">${item.details.map((detail) => escapeHtml(detail)).join("<br/>")}</div>
      </div>
    `).join("") : `<div class="muted">No stage-level differences were detected.</div>`}
  `
}

function renderLineage(detail) {
  const execution = detail.execution_summary
  el("lineage").innerHTML = `
    <div class="lineage-grid">
      <div class="lineage-item">
        <div class="row">
          <strong>Run lineage</strong>
          <span class="pill ${statusClass(detail.summary.status)}">${escapeHtml(detail.summary.status)}</span>
        </div>
        <div class="meta">run ${escapeHtml(detail.summary.id)}</div>
        <div class="meta">root ${escapeHtml(detail.root_run_id || detail.summary.id)}</div>
        <div class="meta">parent ${escapeHtml(detail.parent_run_id || "none")}</div>
      </div>
      <div class="lineage-item">
        <div class="row">
          <strong>Execution context</strong>
          <span class="turn-chip">${escapeHtml(execution?.adapter || "unknown adapter")}</span>
        </div>
        <div class="meta">${escapeHtml(execution?.repo_path || execution?.worktree_path || execution?.cwd || "No repo or cwd persisted")}</div>
        <div class="meta">branch ${escapeHtml(execution?.branch || "not captured")}</div>
      </div>
    </div>
  `
}

function renderFlow(detail) {
  const transitions = detail.transitions || []
  const checkpoints = detail.checkpoints || []
  el("flow").innerHTML = `
    <div class="flow-grid">
      <div class="flow-item">
        <div class="row">
          <strong>Transitions</strong>
          <span class="turn-chip">${transitions.length}</span>
        </div>
        ${transitions.length ? transitions.slice(0, 8).map((item) => `
          <div class="meta">${escapeHtml(item.from_node_id || "start")} → ${escapeHtml(item.to_node_id)}${item.branch ? ` • branch ${escapeHtml(item.branch)}` : ""} • ${item.produced_count} produced</div>
        `).join("") : `<div class="muted">No workflow transitions were persisted.</div>`}
      </div>
      <div class="flow-item">
        <div class="row">
          <strong>Checkpoints</strong>
          <span class="turn-chip">${checkpoints.length}</span>
        </div>
        ${checkpoints.length ? checkpoints.slice(0, 8).map((item) => `
          <div class="meta">${escapeHtml(item.reason)} • ${item.ready_count} ready • ${item.completed_count} completed${item.last_stage_id ? ` • after ${escapeHtml(item.last_stage_id)}` : ""}</div>
        `).join("") : `<div class="muted">No checkpoints were persisted.</div>`}
      </div>
    </div>
  `
}

function renderArtifacts(artifacts) {
  el("artifacts").innerHTML = artifacts.map((artifact) => `
    <div class="artifact-item">
      <div class="row">
        <strong>${escapeHtml(artifact.title)}</strong>
        <span class="turn-chip">${escapeHtml(artifact.kind)}</span>
      </div>
      <div class="meta">${escapeHtml(artifact.source || artifact.stage || "unknown source")} • ${artifact.estimated_tokens != null ? `${formatNumber(artifact.estimated_tokens)} tokens est.` : "no token estimate"} • lineage ${artifact.lineage_count}</div>
      <div class="meta">${escapeHtml(artifact.preview)}</div>
      <details>
        <summary>Artifact id</summary>
        <pre>${escapeHtml(artifact.id)}</pre>
      </details>
    </div>
  `).join("") || `<div class="muted">This run did not persist any artifacts.</div>`
}

function renderHero(detail) {
  el("run-title").textContent = detail.summary.workflow_name
  el("run-subtitle").textContent = `${detail.summary.status} • ${formatDuration(detail.summary.duration_ms)} • ${detail.summary.stage_count} stages`
  el("run-path").textContent = detail.summary.path
  el("hero-meta").innerHTML = [
    ["Model calls", formatNumber(detail.summary.call_count)],
    ["Tokens", `${formatNumber(detail.summary.input_tokens)} in / ${formatNumber(detail.summary.output_tokens)} out`],
    ["Child runs", String(detail.summary.child_run_count)],
    ["Started", detail.summary.started_at],
  ].map(([label, value]) => `<div class="card"><div class="eyebrow">${escapeHtml(label)}</div><div>${escapeHtml(value)}</div></div>`).join("")
}

function renderStages(stages) {
  el("stages").innerHTML = stages.map((stage) => `
    <div class="stage-row">
      <div class="row">
        <strong>${escapeHtml(stage.node_id)}</strong>
        <span class="pill ${statusClass(stage.outcome || stage.status)}">${escapeHtml(stage.outcome || stage.status)}</span>
      </div>
      <div class="meta">${formatDuration(stage.duration_ms)} • ${stage.attempt_count} attempts • ${stage.artifact_count} artifacts</div>
      ${stage.verification_summary ? `<div class="meta">${escapeHtml(stage.verification_summary)}</div>` : ""}
    </div>
  `).join("")
}

function renderActivities(activities) {
  el("activity").innerHTML = activities.slice(0, 40).map((item) => `
    <div class="activity-item">
      <div class="row">
        <strong>${escapeHtml(item.label)}</strong>
        <span>${formatDuration(item.duration_ms)}</span>
      </div>
      <div class="meta">${escapeHtml(item.kind)} • +${formatDuration(item.started_offset_ms)}${item.stage_node_id ? ` • ${escapeHtml(item.stage_node_id)}` : ""}</div>
      <div class="meta">${escapeHtml(item.summary)}</div>
    </div>
  `).join("") || `<div class="muted">No span activity captured for this run.</div>`
}

function renderStory(story) {
  el("story").innerHTML = story.map((section, index) => `
    <div class="story-item">
      <div class="row">
        <strong>${escapeHtml(section.title)}</strong>
        <span class="pill running">${escapeHtml(section.role)}</span>
      </div>
      <div class="meta">${escapeHtml(section.scope)} • ${escapeHtml(section.source)}</div>
      <div class="meta">${escapeHtml(section.preview)}</div>
      <details ${index < 2 ? "open" : ""}>
        <summary>Expand full text</summary>
        <pre>${escapeHtml(section.text)}</pre>
      </details>
    </div>
  `).join("") || `<div class="muted">No human-visible transcript sections were saved for this run.</div>`
}

function renderChildren(children) {
  el("children").innerHTML = children.map((child) => `
    <div class="child-item">
      <div class="row">
        <strong>${escapeHtml(child.worker_name)}</strong>
        <span class="pill ${statusClass(child.status)}">${escapeHtml(child.status)}</span>
      </div>
      <div class="meta">${escapeHtml(child.task)}</div>
      <div class="meta">${escapeHtml(child.run_path || child.run_id || "No persisted child run path")}</div>
      ${child.run_path ? `<button class="run-card-link" data-child-path="${encodeURIComponent(child.run_path)}">Open child run</button>` : ""}
    </div>
  `).join("") || `<div class="muted">No delegated child runs for this run.</div>`
  el("children").querySelectorAll("[data-child-path]").forEach((node) => {
    node.addEventListener("click", () => {
      selectRun(decodeURIComponent(node.dataset.childPath))
    })
  })
}

function renderTurns(steps) {
  el("turns").innerHTML = steps.map((step) => `
    <div class="turn-item">
      <div class="row">
        <strong>Step ${step.call_index}</strong>
        <span class="pill running">iteration ${step.iteration}</span>
      </div>
      <div class="meta">${escapeHtml(step.model)}${step.provider ? ` • ${escapeHtml(step.provider)}` : ""}</div>
      <div class="meta">${escapeHtml(step.summary)}</div>
      <div class="turn-chip-row">
        <span class="turn-chip">${step.total_messages} msgs</span>
        <span class="turn-chip">${step.kept_messages} kept</span>
        <span class="turn-chip">${step.added_messages} added</span>
        ${step.input_tokens != null ? `<span class="turn-chip">${formatNumber(step.input_tokens)} in / ${formatNumber(step.output_tokens || 0)} out</span>` : ""}
        ${step.span_id != null ? `<span class="turn-chip">span ${step.span_id}</span>` : ""}
      </div>
      <div class="turn-grid">
        <div class="turn-panel">
          <h4>What changed right now</h4>
          ${step.system_prompt ? `<div class="meta">Always-on instructions present</div>` : ""}
          ${step.added_context.length ? step.added_context.map((msg) => `
            <div class="turn-message">
              <div class="role">${escapeHtml(msg.role)}</div>
              <pre>${escapeHtml(msg.content)}</pre>
            </div>
          `).join("") : `<div class="muted">No newly added context captured for this step.</div>`}
        </div>
        <div class="turn-panel">
          <h4>What happened next</h4>
          ${step.tool_calls.length ? `<div class="turn-chip-row">${step.tool_calls.map((tool) => `<span class="turn-chip">${escapeHtml(tool)}</span>`).join("")}</div>` : `<div class="muted">No tool calls recorded.</div>`}
          ${step.response_text ? `<details><summary>Expand full reply</summary><pre>${escapeHtml(step.response_text)}</pre></details>` : `<div class="muted">No response text persisted for this step.</div>`}
          ${step.thinking ? `<details><summary>Expand reasoning notes</summary><pre>${escapeHtml(step.thinking)}</pre></details>` : ``}
        </div>
      </div>
    </div>
  `).join("") || `<div class="muted">No saved model transcript sidecar found for this run.</div>`
}

function renderFlamegraph(detail) {
  const root = el("flamegraph")
  const total = Math.max(...detail.spans.map((span) => span.end_ms), detail.summary.duration_ms || 1, 1)
  const stageBars = detail.stages.reduce((acc, stage) => {
    const start = acc.offset
    const duration = stage.duration_ms || 0
    acc.items.push({ label: stage.node_id, kind: "stage", left: pct(start, total), width: pct(duration, total), subtitle: formatDuration(duration) })
    acc.offset += duration
    return acc
  }, { items: [], offset: 0 }).items
  const spanBars = detail.spans.map((span) => ({
    label: span.label,
    kind: ["llm_call", "tool_call", "pipeline"].includes(span.kind) ? span.kind : "other",
    left: pct(span.start_ms, total),
    width: pct(span.duration_ms, total),
    subtitle: `${span.kind} • ${formatDuration(span.duration_ms)}`,
    top: span.lane * 40 + 10,
  }))
  root.innerHTML = `
    ${renderLane("Workflow stages", "Big-picture progress across the run", stageBars, 56)}
    ${renderLane("Nested spans", "Runtime calls and operations inside those stages", spanBars, Math.max(56, (Math.max(0, ...detail.spans.map((span) => span.lane)) + 1) * 40 + 20), true)}
  `
}

function renderLane(title, subtitle, items, height, useCustomTop = false) {
  return `
    <div class="lane">
      <div class="lane-header">
        <div><strong>${escapeHtml(title)}</strong><div class="muted">${escapeHtml(subtitle)}</div></div>
      </div>
      <div class="track" style="height:${height}px">
        <div class="grid">${Array.from({ length: 12 }, () => "<span></span>").join("")}</div>
        ${items.map((item) => `
          <div class="bar ${item.kind}" title="${escapeHtml(item.label)} • ${escapeHtml(item.subtitle)}"
               style="left:${item.left}%;width:${Math.max(item.width, 1.5)}%;top:${useCustomTop ? item.top : 10}px">
            ${escapeHtml(item.label)} <span class="muted">${escapeHtml(item.subtitle)}</span>
          </div>
        `).join("")}
      </div>
    </div>
  `
}

function pct(value, total) {
  return ((value / Math.max(total, 1)) * 100).toFixed(3)
}

function renderDetail(detail) {
  state.detail = detail
  el("empty").classList.add("hidden")
  el("detail").classList.remove("hidden")
  renderHero(detail)
  renderInsights(detail.insights)
  renderLineage(detail)
  renderFlow(detail)
  renderCompare(detail)
  renderFlamegraph(detail)
  renderStages(detail.stages)
  renderActivities(detail.activities)
  renderArtifacts(detail.artifacts)
  renderTurns(detail.transcript_steps)
  renderStory(detail.story)
  renderChildren(detail.child_runs)
}

async function loadRuns() {
  const data = await fetchJson("/api/runs")
  state.runs = data.runs
  const query = el("search").value.toLowerCase()
  state.filteredRuns = data.runs.filter((run) => {
    const haystack = `${run.workflow_name} ${run.status} ${run.models.join(" ")} ${run.path}`.toLowerCase()
    return haystack.includes(query)
  })
  renderStats(data.stats)
  renderRuns()

  if (!state.selectedPath && state.filteredRuns[0]) {
    selectRun(state.filteredRuns[0].path)
  } else if (state.selectedPath && !state.filteredRuns.some((run) => run.path === state.selectedPath)) {
    state.selectedPath = null
    if (state.filteredRuns[0]) selectRun(state.filteredRuns[0].path)
  }
}

async function selectRun(path) {
  state.selectedPath = path
  state.compare = null
  renderRuns()
  const detail = await fetchJson(`/api/run?path=${encodeURIComponent(path)}`)
  renderDetail(detail)
}

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
}

function init() {
  el("search").addEventListener("input", loadRuns)
  loadRuns()
  state.poll = window.setInterval(async () => {
    try {
      await loadRuns()
      if (state.selectedPath) {
        const detail = await fetchJson(`/api/run?path=${encodeURIComponent(state.selectedPath)}`)
        renderDetail(detail)
      }
    } catch (error) {
      console.warn("portal refresh failed", error)
    }
  }, 4000)
}

init()
