# Step-Judge Experiment Report

Source: `experiments/step-judge/results/20260523-192056/`

## Cells

| Cell | Runs | Pass | Pass% | Cost USD | Input tok | Output tok |
|---|---:|---:|---:|---:|---:|---:|
| baseline-cheap | 6 | 3 | 50% | 0.31429199999999996 | 45069 | 11939 |
| symmetric-cheap | 6 | 3 | 50% | 0.28730399999999995 | 35268 | 12100 |
| asymmetric | 6 | 5 | 83% | 0.143058 | 34581 | 2621 |
| symmetric-strong | 6 | 5 | 83% | 0.326739 | 83983 | 4986 |

## Lift vs baseline-cheap

| Cell | Lift (pp) |
|---|---:|
| symmetric-cheap | 0 |
| asymmetric | 33 |
| symmetric-strong | 33 |

## Probes

| Probe | Runs | Pass | Pass% | Cost USD |
|---|---:|---:|---:|---:|
| probe-rubric-adversarial | 6 | 3 | 50% | 0.186054 |
| probe-transcript-shape-retain | 6 | 3 | 50% | 0.186207 |

## Go / no-go

Compare lift to the criteria in `experiments/step-judge/README.md`:

- asymmetric lift ≥ 15pp at ≤3× baseline cost → GO (recommended opt-in)
- symmetric-cheap lift ≥ 10pp at ≤2× baseline cost → GO (both presets)
- 5pp ≤ lift < threshold → SHIP AS OPT-IN with mixed-evidence note
- lift < 5pp or degraded → NO-GO; ship primitive @experimental, file follow-up
