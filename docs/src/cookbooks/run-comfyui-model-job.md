# Run a FLUX.2 Klein image job with ComfyUI

This guide runs Harn's checked-in image example against a local or remote GPU.
The result is a PNG stored by SHA-256 plus a receipt that can be replayed
without ComfyUI.

## Install the model

Install ComfyUI, then follow its
[FLUX.2 Klein guide](https://docs.comfy.org/tutorials/flux/flux-2-klein). The
example expects these filenames:

- `models/text_encoders/qwen_3_4b.safetensors`
- `models/diffusion_models/flux-2-klein-4b-fp8.safetensors`
- `models/vae/flux2-vae.safetensors`

The [FLUX.2 Klein 4B model card](https://huggingface.co/black-forest-labs/FLUX.2-klein-4B)
lists the Apache-2.0 license and GPU memory requirements.

Start ComfyUI on `127.0.0.1:8188`. ComfyUI's API has no authentication, so do
not bind it to a public interface.

For a remote GPU, forward the loopback port:

```sh
ssh -N -L 8188:127.0.0.1:8188 gpu-host
```

Confirm that the forwarded service responds:

```sh
curl --fail http://127.0.0.1:8188/system_stats
```

## Run the image job

From the Harn repository root:

```sh
COMFYUI_URL=http://127.0.0.1:8188 \
MODEL_JOB_IMAGE_PROMPT='A simple blue botanical emblem on a warm cream background, no text' \
harn run --no-sandbox examples/model-jobs/flux2-klein.harn
```

The example prints lifecycle events to stderr and a JSON receipt to stdout. The
first `assets` entry contains the verified PNG path and its
`asset://sha256/...` identity.

This image came from the command above with seed `42` on an RTX 5090:

![A flat cornflower-blue botanical emblem generated through the Harn model-job adapter](./assets/model-job-flux2-klein-proof.png)

The example uses `--no-sandbox` because it is a standalone file that downloads
output from a loopback service. A packaged application should grant only its
ComfyUI origin and output directory. See [Sandboxing](../sandboxing.md) for the
filesystem and network policy controls.

Set `MODEL_JOB_IMAGE_WIDTH`, `MODEL_JOB_IMAGE_HEIGHT`, or
`MODEL_JOB_IMAGE_SEED` to override the
1024×1024 size and seed `42`. Keep each dimension supported by the model and
available GPU memory.

## Test without a GPU

Use `model_job_fake_backend` with an exact list of observations. The normal run
loop still emits progress and stores output, but it makes no HTTP request.

Use `harness.testing.http_mock` to test a ComfyUI adapter response. This crosses
the real submit, history, and output-decoding path without starting ComfyUI.
The conformance fixture at
`conformance/tests/stdlib/model_job_comfyui.harn` is a complete example.

## Replay a receipt

Pass a completed receipt to `model_job_replay_backend`, then call
`model_job_run_result` with the original request. Replay emits the recorded
states and reads the recorded assets. A changed request or asset fails with a
typed error.

Read the [model-job reference](../stdlib/model-jobs.md) for the request, event,
receipt, and error fields.
