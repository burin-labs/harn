# Eval suite regression notifier

Cron trigger recipe that runs an eval pack, compares the current rows with the
latest prior ledger commit through `std/eval/stats::regression_gate`, and posts
a Slack digest only when the gate flips to `regression` or the paired delta
confirms an `improved` result.

The scheduled-eval primitive can run a pack directly with `eval_pack://...`.
This recipe uses a local trigger handler because it adds the post-run gate and
Slack emission step, but the eval execution still goes through `eval_pack_run`.

The Slack post goes through `harn-slack-connector/default::post_eval_verdict`,
which delegates to the connector's existing approved `chat.postMessage` path.
Customize the defaults in `lib.harn`, or pass an override config at
`provider_payload.raw.config` from the cron deployment wrapper. Store the bot
token at `slack/bot-token`; tests pass a mocked token and mocked Slack API base
URL.

## Verify

```sh
harn install
harn check lib.harn
harn test tests/notifier.harn
```
