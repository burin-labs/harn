def positive_integer:
  type == "number" and floor == . and . > 0;

def build_identity:
  (keys | sort) == ["aot_embedded", "codegen_units", "lto", "profile", "rustc", "strip"] and
  (.profile | type == "string" and length > 0) and
  (.codegen_units | positive_integer) and
  (.lto | type == "string") and
  (.strip | type == "string") and
  (.rustc | type == "string" and length > 0) and
  (.aot_embedded | type == "boolean");

def baseline:
  (keys | sort) == ["build_identity", "bytes", "observed_at", "source_sha", "version"] and
  (.version | type == "string" and test("^[0-9]+[.][0-9]+[.][0-9]+$")) and
  (.source_sha | type == "string" and test("^[0-9a-f]{40}$")) and
  (.bytes | positive_integer) and
  (.observed_at | type == "string" and test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$")) and
  (.build_identity | type == "object" and build_identity);

def growth_policy:
  (keys | sort) == ["warn_bytes", "warn_percent_hundredths"] and
  (.warn_bytes | positive_integer) and
  (.warn_percent_hundredths | positive_integer);

# An acceptance is scoped to the baseline it was written against, so refreshing
# the baseline forces every stale acceptance to be dropped in the same change.
def accepted_growth($baseline_sha):
  (keys | sort) == ["against_baseline_sha", "bytes", "reason"] and
  (.against_baseline_sha == $baseline_sha) and
  (.bytes | positive_integer) and
  (.reason | type == "string" and (sub("^\\s+"; "") | sub("\\s+$"; "") | length) > 0);

# The fuse is an emergency distribution ceiling, not a ratchet: at least 10% and
# at most 15% above the accepted baseline. Tighter and it becomes the
# release-day byte cliff this policy replaced; looser and it stops being a fuse.
def target_policy:
  # Bound here, not at the `all(...)` call: inside `all` the input is the
  # element, so `.baseline.source_sha` there would read the acceptance.
  (.baseline.source_sha // "") as $baseline_sha |
  (keys | sort) == ["accepted_growth", "baseline", "distribution_fuse_bytes", "growth", "target"] and
  (.target | type == "string" and length > 0) and
  (.distribution_fuse_bytes | positive_integer) and
  (.baseline | type == "object" and baseline) and
  (.growth | type == "object" and growth_policy) and
  (.accepted_growth | type == "array" and all(.[]; accepted_growth($baseline_sha))) and
  (.distribution_fuse_bytes >= ((.baseline.bytes * 110 / 100) | floor)) and
  (.distribution_fuse_bytes <= ((.baseline.bytes * 115 / 100) | floor));

.default_target as $default_target |
select(
  (keys | sort) == ["default_target", "schema_version", "targets"] and
  .schema_version == 2 and
  (.default_target | type == "string" and length > 0) and
  (.targets | type == "array" and length > 0 and all(.[]; target_policy)) and
  ([.targets[].target] | unique | length) == (.targets | length) and
  ([.targets[].target] | index($default_target)) != null
) |
[.targets[] | select(.target == $target)] |
select(length == 1) |
.[0].distribution_fuse_bytes
