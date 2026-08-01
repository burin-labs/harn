# Why Harn has model jobs

An image generator and a speech service look different at their HTTP edges.
Inside an application, they have the same awkward shape: submit work, wait,
show progress, collect bytes, and handle failure or cancellation.

Harn calls that shape a [model job](./glossary.md#durable-state). The runtime
owns its lifecycle. A backend translates one service into Harn's five states.
The application sees the same events and receipt whether the service runs on a
local GPU or behind a hosted API.

This boundary keeps model engines out of the language. PyTorch, ComfyUI, and
hosted inference services already own tensors, kernels, model files, and GPU
memory. Rebuilding those systems in Harn would add a second, weaker inference
stack. Harn instead owns the application facts that must survive a provider
change:

- the request and selected backend;
- legal state changes and progress events;
- the exact output bytes and their origin;
- cancellation, timeout, failure, and offline replay.

## Jobs are not agent turns

An agent turn asks a language model to reason, call tools, and continue a
conversation. A model job asks a service to produce a fixed result. It may run
for minutes, but it does not own a conversation.

An agent can submit a model job. A GUI can submit the same job directly. Both
use [`std/model_job`](../stdlib/model-jobs.md), so progress, receipts, and replay
do not depend on the host that drew them.

## Outputs become media assets

A backend can return bytes, a local path, or a download URL. Harn reads that
source once, checks the file signature against the declared MIME type, and
stores the bytes under their SHA-256 digest. The resulting
[media asset](./glossary.md#durable-state) has a portable `asset://sha256/...`
identity and a local materialization path.

Content addressing gives replay a hard failure mode. If the bytes have changed,
the digest check fails. Harn does not quietly contact the original provider and
produce a different result.

For the exact types and state rules, read the
[model-job reference](../stdlib/model-jobs.md). To run a local image model, use
the [ComfyUI how-to guide](../cookbooks/run-comfyui-model-job.md). To run a
hosted model, use the [OpenAI image how-to guide](../cookbooks/run-openai-image-job.md).
