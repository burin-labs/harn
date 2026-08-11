# Closed contract for .github/release-warm-build-budget.json.
# Validates shape and emits one line per target: target=budget_seconds.
def positive_integer:
  type == "number" and floor == . and . > 0;

def target_policy:
  (keys | sort) == ["baseline_seconds", "budget_seconds", "target", "warn_seconds"]
    and (.target | type == "string" and length > 0)
    and all([.baseline_seconds, .warn_seconds, .budget_seconds][]; positive_integer)
    and .warn_seconds >= .baseline_seconds
    and .budget_seconds >= .warn_seconds;

select(
  (keys | sort) == ["baseline", "metric", "schema_version", "targets"]
    and .schema_version == 1
    and .metric == "build_step_wall_seconds"
    and (.baseline | type == "object")
    and ((.baseline | keys | sort) == ["note", "observed_at", "run_id", "url", "version"])
    and (.baseline.version | type == "string" and length > 0)
    and (.baseline.run_id | type == "string" and test("^[0-9]+$"))
    and (.baseline.url | type == "string" and startswith("https://github.com/"))
    and (.baseline.observed_at | type == "string" and length > 0)
    and (.baseline.note | type == "string" and length > 0)
    and (.targets | type == "array" and length > 0)
    and ([.targets[].target] | length == (unique | length))
    and all(.targets[]; target_policy)
) |
.targets[] |
"\(.target)=\(.budget_seconds)"
