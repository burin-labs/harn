def positive_integer:
  type == "number" and floor == . and . > 0;

def baseline:
  (keys | sort) == ["bytes", "observed_at", "source_sha", "version"] and
  (.version | type == "string" and test("^[0-9]+[.][0-9]+[.][0-9]+$")) and
  (.source_sha | type == "string" and test("^[0-9a-f]{40}$")) and
  (.bytes | positive_integer) and
  (.observed_at | type == "string" and test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$"));

def target_policy:
  (keys | sort) == ["baseline", "budget_bytes", "target"] and
  (.target | type == "string" and length > 0) and
  (.budget_bytes | positive_integer) and
  (.baseline | type == "object" and baseline) and
  (.budget_bytes - .baseline.bytes >= 1048576) and
  (.budget_bytes - .baseline.bytes <= 4194304);

.default_target as $default_target |
select(
  (keys | sort) == ["default_target", "schema_version", "targets"] and
  .schema_version == 1 and
  (.default_target | type == "string" and length > 0) and
  (.targets | type == "array" and length > 0 and all(.[]; target_policy)) and
  ([.targets[].target] | unique | length) == (.targets | length) and
  ([.targets[].target] | index($default_target)) != null
) |
[.targets[] | select(.target == $target)] |
select(length == 1) |
.[0].budget_bytes
