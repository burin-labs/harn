# Scheduled eval suite

Cron trigger that runs an eval pack directly through `eval_pack://scheduled-regression`.
The dispatcher applies the trigger budget, retry, dedupe, DLQ, replay, and
concurrency controls before calling `eval_pack_run`.

## Verify

```sh
harn check lib.harn
harn test package --evals
```
