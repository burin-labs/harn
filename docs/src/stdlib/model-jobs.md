# Model-job reference

Import `std/model_job` for the public model-job, media-asset, ComfyUI, and test
APIs.

```harn
import {
  ModelBackend,
  ModelJobRequest,
  ModelJobRunOptions,
  model_job_run_result,
} from "std/model_job"
```

The [model-job explanation](../concepts/model-jobs.md) describes why this
boundary exists. This page lists its contracts.

## Request

`ModelJobRequest` has these fields:

| Field | Type | Meaning |
|---|---|---|
| `id` | `string` | Caller-generated request ID. |
| `task` | `ModelTask` | `image.generate`, `image.edit`, `audio.generate`, `video.generate`, `embedding`, or `custom`. |
| `prompt` | `string` | Required except for `embedding`. |
| `output` | `ModelOutputSpec` | Required MIME type and optional width, height, duration, or count. |
| `model` | `string?` | Model selected by the caller. |
| `inputs` | `list<MediaAsset>?` | Input assets for editing or other conditional work. |
| `seed` | `int?` | Reproducible random seed when the backend supports one. |
| `params` | `dict?` | Backend-specific settings. |
| `metadata` | `dict?` | Application data carried with the request. |

`model_job_request_result` validates the common fields before the backend is
called. `model_job_request_digest` hashes the fields that affect model output.
Replay requires the same digest.

## Backend

A backend has one ID and three functions:

```harn
type ModelBackend = {
  id: string,
  submit: fn(Harness, ModelJobRequest) -> Result<ModelJobObservation, ModelJobError>,
  inspect: fn(Harness, ModelJob) -> Result<ModelJobObservation, ModelJobError>,
  cancel: fn(Harness, ModelJob) -> Result<ModelJobObservation, ModelJobError>,
}
```

`submit` returns the first observation. `inspect` returns the latest provider
state. `cancel` requests cancellation and returns the resulting observation.
The backend may use `harness.net`, a connector, or a local process.

Map every provider status through `model_job_state_result`. An unknown status is
an `invalid_state` error, not a running job.

## State transitions

The closed state set is `queued`, `running`, `succeeded`, `failed`, and
`canceled`.

| Current state | Allowed next states |
|---|---|
| `queued` | `running`, `succeeded`, `failed`, `canceled` |
| `running` | `succeeded`, `failed`, `canceled` |
| terminal state | the same state only |

`succeeded`, `failed`, and `canceled` are terminal. A canceled job cannot later
succeed. `model_job_transition_result` enforces this rule for all backends.

## Run options and events

`model_job_run_result(harness, backend, request, options)` submits and polls one
job. `ModelJobRunOptions` accepts:

| Field | Default | Meaning |
|---|---:|---|
| `timeout_ms` | `300000` | Total polling deadline. |
| `interval_ms` | `500` | Delay between inspections. |
| `max_attempts` | `0` | Inspection limit; `0` has no attempt limit. |
| `asset_root` | runtime asset root | Content-addressed output directory. |
| `session_id` | none | Agent session that receives `model_job` transcript events. |
| `on_event` | none | Callback for UI or CLI progress. |

Events are ordered. Their kinds are `submitted`, `state_changed`, `progress`,
`output`, and `failed`. Every event includes the request ID, job ID, backend,
state, and monotonic timestamp.

After submission, polling, timeout, transition, and asset-storage errors emit a
terminal `failed` event before `model_job_run_result` returns the typed error.
This gives transcript and UI consumers a terminal state even when no receipt is
created.

## Run a job from an interactive app

`model_job_run_result` is convenient for a CLI or batch step that can wait. An
interactive app should retain a `ModelJobRun` and advance the job one step
at a time:

| Function | Work performed |
|---|---|
| `model_job_submit_result` | Validate and submit once; return the first job and event. |
| `model_job_step_result` | Inspect once and append the checked state or progress event. |
| `model_job_finish_result` | Store a finished run's outputs as verified assets. |
| `model_job_cancel_run_result` | Cancel a non-terminal run and append its event. |

`ModelJobRun` can be encoded as JSON. It contains the current job, ordered events,
start time, and inspection count. Calling `model_job_step_result` on a finished
run returns it unchanged. That makes a scheduled check safe when it crosses
with a cancel or final response. Each step also enforces `timeout_ms` and
`max_attempts`, so remembered jobs settle instead of polling indefinitely after
a provider loses their remote state.

The synchronous `model_job_run_result` uses these same functions, so Harn has
one implementation for both waiting and interactive callers.

## Receipt and media asset

A successful `ModelJobReceipt` contains the final job, ordered events, request
digest, backend ID, and verified assets.

Each `MediaAsset` includes:

- an `asset://sha256/<digest>` URI and SHA-256 digest;
- MIME type, byte size, kind, and current path;
- optional dimensions, duration, producing job, and metadata.

`media_asset_store_result` rejects bytes whose signature does not match the
declared MIME type. `media_asset_verify_result` re-reads the file and rejects a
changed digest, size, MIME type, or identity.

## Test and replay backends

`model_job_fake_backend(id, observations)` consumes a fixed observation list.
It has no network effects. Use it for job-state and UI tests.

`model_job_replay_backend(receipt)` replays recorded states and asset paths. It
rejects a different request digest and never falls back to the live provider.

`comfyui_backend(endpoint, build_workflow, options)` implements the same
interface over ComfyUI. Its graph builder is separate, so any API-format ComfyUI
workflow can use the backend. `comfyui_flux2_klein_workflow` is the included
text-to-image graph. When a request has input assets, the backend verifies and
uploads them before building the graph; their ComfyUI names are available in
`request.params.comfy_input_names`. Use
`comfyui_flux2_klein_edit_workflow` for a one-image FLUX.2 Klein edit. Its graph
follows ComfyUI's
[official Klein image-edit template](https://docs.comfy.org/tutorials/flux/flux-2-klein).

`openai_responses_image_backend(options)` completes generation or editing in
its `submit` call. It uses the OpenAI Responses API image-generation tool,
accepts verified media assets as edit inputs, and returns base64 output as
normal model-job assets. The API key is a required option and is never added to
events or receipts.

Run the local backend with the
[ComfyUI how-to guide](../cookbooks/run-comfyui-model-job.md), or run the hosted
backend with the [OpenAI image how-to guide](../cookbooks/run-openai-image-job.md).
