import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { IntlProvider } from "react-intl"
import { afterEach, describe, expect, it, vi } from "vitest"

import { LaunchPanel } from "./LaunchPanel"

afterEach(() => {
  cleanup()
})

describe("LaunchPanel", () => {
  it("submits playground launches with provider, model, and env overrides", async () => {
    const onLaunch = vi.fn(async () => {})

    render(
      <IntlProvider locale="en">
        <LaunchPanel
          meta={{ workspace_root: "/workspace/harn", run_dir: ".harn-runs/portal-demo" }}
          llmOptions={{
            preferred_provider: "openai",
            preferred_model: "gpt-4.1-mini",
            providers: [
              {
                name: "openai",
                base_url: "https://api.openai.com/v1",
                base_url_env: null,
                auth_style: "bearer",
                auth_envs: ["OPENAI_API_KEY"],
                auth_configured: true,
                viable: true,
                local: false,
                models: ["gpt-4.1-mini"],
                aliases: [],
                default_model: "gpt-4.1-mini",
              },
            ],
          }}
          targets={[{ path: "examples/demo.harn", group: "examples" }]}
          jobs={[]}
          onLaunch={onLaunch}
          onOpenRun={() => {}}
        />
      </IntlProvider>,
    )

    fireEvent.change(screen.getByLabelText("Task"), {
      target: { value: "Draft a release note" },
    })
    fireEvent.change(screen.getByLabelText("Env JSON overrides"), {
      target: { value: '{"OPENAI_API_KEY":"test-key"}' },
    })
    fireEvent.click(screen.getAllByRole("button", { name: "Run now" }).at(-1)!)

    await waitFor(() => {
      expect(onLaunch).toHaveBeenCalledWith({
        env: { OPENAI_API_KEY: "test-key" },
        file_path: undefined,
        model: "gpt-4.1-mini",
        provider: "openai",
        source: undefined,
        task: "Draft a release note",
      })
    })
  })

  it("renders launch workspace details and open-run actions", async () => {
    const onOpenRun = vi.fn()

    render(
      <IntlProvider locale="en">
        <LaunchPanel
          meta={{ workspace_root: "/workspace/harn", run_dir: ".harn-runs/portal-demo" }}
          llmOptions={{
            preferred_provider: "openai",
            preferred_model: "gpt-4.1-mini",
            providers: [
              {
                name: "openai",
                base_url: "https://api.openai.com/v1",
                base_url_env: null,
                auth_style: "bearer",
                auth_envs: ["OPENAI_API_KEY"],
                auth_configured: true,
                viable: true,
                local: false,
                models: ["gpt-4.1-mini"],
                aliases: [],
                default_model: "gpt-4.1-mini",
              },
            ],
          }}
          targets={[]}
          jobs={[
            {
              id: "job-1",
              mode: "playground",
              target_label: "playground: debug prompt",
              status: "completed",
              started_at: "started-1",
              finished_at: "finished-1",
              exit_code: 0,
              logs: "completed",
              discovered_run_paths: ["playground/job-1/run.json"],
              workspace_dir: "/tmp/harn/job-1",
              transcript_path: "/tmp/harn/job-1/run-llm/llm_transcript.jsonl",
            },
          ]}
          onLaunch={async () => {}}
          onOpenRun={onOpenRun}
        />
      </IntlProvider>,
    )

    expect(screen.getByText("/tmp/harn/job-1")).toBeInTheDocument()
    expect(screen.getByText("/tmp/harn/job-1/run-llm/llm_transcript.jsonl")).toBeInTheDocument()

    await userEvent.click(screen.getByRole("button", { name: "Open run playground/job-1/run.json" }))
    expect(onOpenRun).toHaveBeenCalledWith("playground/job-1/run.json")
  })

  it("allows custom model input and endpoint overrides for local providers", async () => {
    const onLaunch = vi.fn(async () => {})

    render(
      <IntlProvider locale="en">
        <LaunchPanel
          meta={{ workspace_root: "/workspace/harn", run_dir: ".harn-runs/portal-demo" }}
          llmOptions={{
            preferred_provider: "local",
            preferred_model: "gpt-4o",
            providers: [
              {
                name: "local",
                base_url: "http://localhost:8000",
                base_url_env: "LOCAL_LLM_BASE_URL",
                auth_style: "none",
                auth_envs: [],
                auth_configured: true,
                viable: true,
                local: true,
                models: ["gpt-4o"],
                aliases: [],
                default_model: "gpt-4o",
              },
            ],
          }}
          targets={[]}
          jobs={[]}
          onLaunch={onLaunch}
          onOpenRun={() => {}}
        />
      </IntlProvider>,
    )

    await userEvent.selectOptions(screen.getAllByLabelText("Model").at(-1)!, "Custom…")
    fireEvent.change(screen.getByLabelText("Custom model"), {
      target: { value: "qwen2.5-coder:latest" },
    })
    fireEvent.change(screen.getAllByLabelText(/Endpoint URL/).at(-1)!, {
      target: { value: "http://192.168.1.40:8000" },
    })
    await userEvent.click(screen.getAllByRole("button", { name: "Run now" }).at(-1)!)

    expect(onLaunch).toHaveBeenCalledWith({
      env: { LOCAL_LLM_BASE_URL: "http://192.168.1.40:8000" },
      file_path: undefined,
      model: "qwen2.5-coder:latest",
      provider: "local",
      source: undefined,
      task: "Summarize the repository in a few bullets.",
    })
  })

  it("rejects null env JSON before launching", async () => {
    const onLaunch = vi.fn(async () => {})

    render(
      <IntlProvider locale="en">
        <LaunchPanel
          meta={{ workspace_root: "/workspace/harn", run_dir: ".harn-runs/portal-demo" }}
          llmOptions={null}
          targets={[]}
          jobs={[]}
          onLaunch={onLaunch}
          onOpenRun={() => {}}
        />
      </IntlProvider>,
    )

    fireEvent.change(screen.getByLabelText("Env JSON overrides"), {
      target: { value: "null" },
    })
    await userEvent.click(screen.getAllByRole("button", { name: "Run now" }).at(-1)!)

    expect(await screen.findByText("Env JSON must be an object")).toBeInTheDocument()
    expect(onLaunch).not.toHaveBeenCalled()
  })

  it("rejects non-string env JSON values before launching", async () => {
    const onLaunch = vi.fn(async () => {})

    render(
      <IntlProvider locale="en">
        <LaunchPanel
          meta={{ workspace_root: "/workspace/harn", run_dir: ".harn-runs/portal-demo" }}
          llmOptions={null}
          targets={[]}
          jobs={[]}
          onLaunch={onLaunch}
          onOpenRun={() => {}}
        />
      </IntlProvider>,
    )

    fireEvent.change(screen.getByLabelText("Env JSON overrides"), {
      target: { value: '{"OPENAI_API_KEY":123}' },
    })
    await userEvent.click(screen.getAllByRole("button", { name: "Run now" }).at(-1)!)

    expect(await screen.findByText("Env JSON values must be strings")).toBeInTheDocument()
    expect(onLaunch).not.toHaveBeenCalled()
  })
})
