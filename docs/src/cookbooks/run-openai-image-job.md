# Run an OpenAI image job

This guide sends one image request through Harn's model-job lifecycle. The
OpenAI Responses API returns the image; Harn verifies and stores it by SHA-256.

## Set the credential

Make `OPENAI_API_KEY` available to the Harn process. Do not put the value in a
script, checked-in configuration, command argument, or receipt.

The API organization may need
[verification](https://help.openai.com/en/articles/10910291-api-organization-verification)
before it can use GPT Image models.

## Run the example

From the Harn repository root:

```sh
MODEL_JOB_IMAGE_PROMPT='A flat blue flower emblem on a cream background, no text' \
harn run --no-sandbox examples/model-jobs/openai-image.harn
```

The example uses `gpt-5.6-sol` with the Responses API image-generation tool and
requests low-quality PNG output. Set `OPENAI_IMAGE_RESPONSE_MODEL` to choose a
different Responses API model that supports the tool. OpenAI selects the GPT
Image model behind that tool.

The [OpenAI image-generation guide](https://developers.openai.com/api/docs/guides/image-generation)
documents supported inputs, output controls, and current pricing. It recommends
the Responses API for conversational or multi-step image editing. Use the Image
API when the application needs a direct, single-request GPT Image model choice.

The receipt is printed to stdout. Its first `assets` entry contains the output
path and `asset://sha256/...` identity.

## Confirm the result

This PNG came from the example's low-quality hosted path on August 1, 2026.
Harn stored 898,270 bytes with SHA-256
`ba1826d69bb02712af24ced31800cc1197b37f963a034a4a8671142407917469`.

![Cornflower-blue botanical emblem generated through the OpenAI model-job adapter](assets/model-job-openai-proof.png)

Check `job.state == "succeeded"`, then call `media_asset_verify_result` on the
first asset before using it as an edit input or copying it to a product-owned
location.

## Edit an image

Set the request task to `image.edit` and pass one or more verified `MediaAsset`
values in `request.inputs`. The adapter sends each asset as a base64 data URL.
It rejects changed asset bytes before the network request.

For a continued Responses API edit, set
`request.params.previous_response_id` to the response ID stored in the first
output asset's metadata. This keeps the prior image in API context.

Read the [model-job reference](../stdlib/model-jobs.md) for lifecycle, error,
and replay behavior.
