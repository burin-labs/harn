# Three-strategy planning comparison

Source: `examples/task_plan/results/runs_combined.jsonl` (45 records).

## Overall (mean across tasks)

| Strategy | trials | pass | validate | workflow_validate | mean_in | mean_out | mean_wall_ms | mean_structured | mean_deterministic |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `baseline` | 15 | 100.0% | 0.0% | 0.0% | 121 | 1655 | 21048 | 18 | 0 |
| `burin_plan` | 15 | 86.6% | 86.6% | 0.0% | 181 | 1159 | 18507 | 6 | 1 |
| `typed_ir` | 15 | 39.9% | 39.9% | 39.9% | 410 | 1392 | 22270 | 2 | 1 |

## Per-model breakdown

### local — `qwen3.6:35b-a3b-coding-nvfp4`

| Strategy | trials | pass | validate | workflow_validate | mean_out_tok | mean_wall_ms |
|---|---:|---:|---:|---:|---:|---:|
| `baseline` | 5 | 100.0% | 0.0% | 0.0% | 1800 | 34886 |
| `burin_plan` | 5 | 100.0% | 100.0% | 0.0% | 1150 | 25423 |
| `typed_ir` | 5 | 20.0% | 20.0% | 20.0% | 1664 | 37983 |

### cerebras — `gpt-oss-120b`

| Strategy | trials | pass | validate | workflow_validate | mean_out_tok | mean_wall_ms |
|---|---:|---:|---:|---:|---:|---:|
| `baseline` | 5 | 100.0% | 0.0% | 0.0% | 1657 | 1109 |
| `burin_plan` | 5 | 100.0% | 100.0% | 0.0% | 847 | 719 |
| `typed_ir` | 5 | 100.0% | 100.0% | 100.0% | 797 | 573 |

### openrouter — `anthropic/claude-sonnet-4-6`

| Strategy | trials | pass | validate | workflow_validate | mean_out_tok | mean_wall_ms |
|---|---:|---:|---:|---:|---:|---:|
| `baseline` | 5 | 100.0% | 0.0% | 0.0% | 1509 | 27152 |
| `burin_plan` | 5 | 60.0% | 60.0% | 0.0% | 1482 | 29380 |
| `typed_ir` | 5 | 0.0% | 0.0% | 0.0% | 1716 | 28254 |

## Per-task breakdown

### baseline

| Task | n | pass | parse | validate | mean_in | mean_out | wall_ms | structured | deterministic | total_nodes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `01_rate_limiter` | 3 | 100.0% | 100.0% | 0.0% | 128 | 1800 | 17864 | 13 | 0 | 0 |
| `02_config_option` | 3 | 100.0% | 100.0% | 0.0% | 122 | 1735 | 18511 | 27 | 0 | 0 |
| `03_failing_test` | 3 | 100.0% | 100.0% | 0.0% | 112 | 1562 | 25465 | 19 | 0 | 0 |
| `04_doc_rename` | 3 | 100.0% | 100.0% | 0.0% | 129 | 1523 | 21130 | 18 | 0 | 0 |
| `05_cross_file_refactor` | 3 | 100.0% | 100.0% | 0.0% | 114 | 1659 | 22273 | 17 | 0 | 0 |

### burin_plan

| Task | n | pass | parse | validate | mean_in | mean_out | wall_ms | structured | deterministic | total_nodes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `01_rate_limiter` | 3 | 66.6% | 66.6% | 66.6% | 188 | 1320 | 18256 | 4 | 2 | 4 |
| `02_config_option` | 3 | 100.0% | 100.0% | 100.0% | 182 | 1084 | 14571 | 7 | 2 | 7 |
| `03_failing_test` | 3 | 100.0% | 100.0% | 100.0% | 173 | 1149 | 19067 | 7 | 2 | 7 |
| `04_doc_rename` | 3 | 66.6% | 66.6% | 66.6% | 190 | 1068 | 19140 | 4 | 1 | 4 |
| `05_cross_file_refactor` | 3 | 100.0% | 100.0% | 100.0% | 175 | 1177 | 21503 | 8 | 2 | 8 |

### typed_ir

| Task | n | pass | parse | validate | mean_in | mean_out | wall_ms | structured | deterministic | total_nodes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `01_rate_limiter` | 3 | 33.3% | 33.3% | 33.3% | 417 | 1498 | 23305 | 1 | 0 | 1 |
| `02_config_option` | 3 | 33.3% | 66.6% | 33.3% | 411 | 1453 | 23964 | 4 | 2 | 4 |
| `03_failing_test` | 3 | 33.3% | 66.6% | 33.3% | 402 | 1298 | 21366 | 4 | 3 | 4 |
| `04_doc_rename` | 3 | 66.6% | 66.6% | 66.6% | 419 | 1190 | 18676 | 2 | 1 | 2 |
| `05_cross_file_refactor` | 3 | 33.3% | 33.3% | 33.3% | 404 | 1523 | 24039 | 1 | 1 | 1 |
