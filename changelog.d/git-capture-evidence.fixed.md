**Expose measured Git output completeness to typed callers (#11455).**
`GitCommandResult` now includes optional `stdout_truncated` and
`stderr_truncated` fields. Existing process adapters that omit this evidence
continue to return `nil`; missing evidence does not become a measured `false`.
