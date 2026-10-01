Reviewer calibration accepts a `--policy` overlay on the running runtime's
bundled policy and records the effective policy fingerprint. `--diagnostics`
adds the reviewer trace to the receipt, each case keeps the reviewer's failure
detail, and `approval_review_policy` exposes the resolved policy to scripts.
