The direct Gemini provider adapter now writes raw request/response sidecars
under `HARN_LLM_TRANSCRIPT_RAW=1`, the same opt-in capture every other
provider route already emits. Previously a direct-Gemini call made requests
and got responses but left zero raw-provider records, so a wire diff against
another route measured nothing on the direct side.
