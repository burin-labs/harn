# Unknown or incomplete evidence never authorizes a source revert.
def failed_assertion($gate):
  select(.name == $gate.job and .status == "completed" and .conclusion == "failure")
  | select(.steps | type == "array")
  | select([.steps[] | select(.status == "completed" and .conclusion == "failure") | .name] == [$gate.step]);
if ($policy | length) != 1 or ($policy[0].source_assertions | type) != "array"
   or ($policy[0].source_assertions | length) == 0
   or (.first | type) != "array" or (.latest | type) != "array" then
  error("invalid recovery policy or job census")
else
  . as $census
  | {eligible: [
      $policy[0].source_assertions[] as $gate
      | [$census.first[] | failed_assertion($gate)] as $first
      | [$census.latest[] | failed_assertion($gate)] as $latest
      | select(($first | length) == 1 and ($latest | length) == 1)
      | {job: $gate.job, step: $gate.step}
    ],
    pending: [.first[], .latest[] | select(.status != "completed") | .name],
    failing: [.latest[] | select(.conclusion == "failure") | .name]}
  | .authorized = ((.eligible | length) > 0 and (.pending | length) == 0)
end
