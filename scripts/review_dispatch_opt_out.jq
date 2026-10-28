# The ready-event dispatcher and the scheduled sweep consume the same label
# policy. Missing or malformed label data must never mean permission to review.
def review_dispatch_opted_out:
  if type != "array" or any(.[]; type != "string") then
    error("review labels are incomplete or malformed")
  else
    any(.[]; ascii_downcase == "skip-auto-review")
  end;

def review_dispatch_event_opted_out:
  .pull_request.labels
  | if type == "array" then map(.name) else error("review labels are missing") end
  | review_dispatch_opted_out;

def review_dispatch_connection_opted_out:
  .labels as $labels
  | if ($labels | type) != "object" or ($labels.nodes | type) != "array"
       or ($labels.totalCount | type) != "number"
       or ($labels.nodes | length) != $labels.totalCount
    then error("review labels are missing or truncated")
    else $labels.nodes | map(.name) | review_dispatch_opted_out
    end;

def review_dispatch_pages_opted_out:
  if type != "array" or length == 0 or any(.[]; type != "array") then
    error("review label pages are missing or malformed")
  else
    add | map(.name) | review_dispatch_opted_out
  end;

def review_dispatch_latest_request:
  if type != "array" or length == 0 or any(.[]; (.check_runs | type) != "array") then
    error("review request pages are missing or malformed")
  else
    [.[].check_runs[]
      | select(.conclusion != "skipped")
      | .started_at | select(. != null)] | max
  end;
