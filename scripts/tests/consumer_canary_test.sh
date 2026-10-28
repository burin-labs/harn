#!/usr/bin/env bash
# The canary must never report green without a consumer run that concluded
# success, and must rehearse exactly the pairing it was given.
set -eEuo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
bash "$root/scripts/tests/consumer_observation_policy_test.sh"
node --test "$root/scripts/tests/consumer_observer_test.mjs" "$root/scripts/tests/consumer_observer_cleanup_test.mjs" "$root/scripts/tests/consumer_observer_terminal_test.mjs" "$root/scripts/tests/consumer_observer_dispatch_test.mjs" "$root/scripts/tests/consumer_observer_window_test.mjs" "$root/scripts/tests/consumer_observer_retirement_test.mjs" "$root/scripts/tests/consumer_observation_budget_test.mjs" "$root/scripts/tests/consumer_observer_budget_entry_test.mjs" "$root/scripts/tests/release_producer_readiness_test.mjs"
node --test "$root/scripts/tests/consumer_pin_test.mjs"
scratch=$(mktemp -d "${TMPDIR:-/tmp}/harn-consumer-canary.XXXXXX")
export RUNNER_TEMP="$scratch"
trap 'rm -rf "$scratch"' EXIT
trap 'printf "Consumer canary assertion failed at line %s\n" "$LINENO" >&2; if [[ -f "$scratch/out" ]]; then cat "$scratch/out" >&2; fi' ERR

# Legacy verdict and renewal controls still cross the actual fresh-admission
# entrypoint. Only authenticated GitHub reads are replaced with local bytes.
cat > "$scratch/parent-fetch.mjs" <<'JS'
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {pathToFileURL} from 'node:url';
if(process.argv[1]?.endsWith('/consumer-observer-dispatch.mjs')){
  const {observerDeclarationName}=await import(pathToFileURL(process.env.CANARY_TEST_ROOT+'/scripts/lib/consumer-observer.mjs'));
  const context=JSON.parse(process.env.OBSERVER_CONTEXT);
  const dir=fs.mkdtempSync(path.join(os.tmpdir(),'harn-canary-parent-'));
  process.on('exit',()=>fs.rmSync(dir,{recursive:true}));
  const file=path.join(dir,'consumer-observer.json'),archive=path.join(dir,'declaration.zip');
  fs.writeFileSync(file,JSON.stringify(context));
  execFileSync('zip',['-j',archive,file],{stdio:'ignore'});
  const bytes=fs.readFileSync(archive);
  const metadata={id:501,name:observerDeclarationName(context),expired:false,size_in_bytes:bytes.length,
    digest:'sha256:'+createHash('sha256').update(bytes).digest('hex'),
    workflow_run:{id:Number(context.parent_run_id),head_sha:context.parent_head_sha}};
  globalThis.fetch=async(url,options)=>{
    if(options.headers.Authorization!=='Bearer fixture-parent')throw new Error('wrong-parent-authority');
    const endpoint=new URL(url);
    if(endpoint.pathname.endsWith('/artifacts'))return {ok:true,json:async()=>({total_count:1,artifacts:[metadata]})};
    if(endpoint.pathname.endsWith('/501/zip'))return {ok:true,arrayBuffer:async()=>bytes};
    if(endpoint.pathname.endsWith('/runs/'+context.parent_run_id))return {ok:true,json:async()=>({
      id:Number(context.parent_run_id),run_attempt:context.parent_run_attempt,
      head_sha:context.parent_head_sha,head_repository:{full_name:context.parent_repository},
      path:process.env.CANARY_RELEASE_PRODUCER_RUN_ID
        ? '.github/workflows/build-release-binaries.yml' : '.github/workflows/consumer-canary.yml',
      status:'in_progress',
    })};
    throw new Error('unexpected-parent-read');
  };
}
if(process.argv[1]?.endsWith('/consumer-observer-failure.mjs')){
  globalThis.fetch=async()=>({ok:true,json:async()=>({total_count:1,workflow_runs:[{id:99,run_attempt:1,status:'queued'}]})});
}
if(process.argv[1]?.endsWith('/consumer-observer-budget.mjs')&&!process.argv.includes('--policy')){
  const {admitChild,observerDeclarationName,observerRunName,observerDispatchActor,readRetry}=await import(
    pathToFileURL(process.env.CANARY_TEST_ROOT+'/scripts/lib/consumer-observer.mjs'));
  readRetry.backoffMs=0;
  const context=JSON.parse(process.env.OBSERVER_CONTEXT);
  const nowFile=path.join(process.env.STUB,'now');
  if(fs.existsSync(nowFile))Date.now=()=>Number(fs.readFileSync(nowFile,'utf8'))*1000;
  const now=Math.floor(Date.now()/1000);
  const iso=n=>new Date(n*1000).toISOString().replace('.000Z','Z');
  const parent={id:Number(context.parent_run_id),run_attempt:context.parent_run_attempt,
    head_sha:context.parent_head_sha,head_repository:{full_name:context.parent_repository},
    path:'.github/workflows/consumer-canary.yml',status:'in_progress',created_at:iso(context.started_at-10)};
  const [status,conclusion]=fs.readFileSync(path.join(process.env.STUB,'run'),'utf8').trim().split(/\s+/);
  const child={id:Number(process.env.CANARY_RUN_ID),run_attempt:1,head_sha:'c'.repeat(40),
    head_repository:{full_name:'acme/widget-host'},path:'.github/workflows/rehearsal.yml',
    event:'workflow_dispatch',status,conclusion,display_title:observerRunName(context),
    actor:{login:observerDispatchActor,type:'Bot'}};
  const expected={repository:'acme/widget-host',workflow:child.path,runId:String(child.id),
    attempt:1,headSha:child.head_sha,sourceRevision:context.source_revision};
  const receipt=admitChild(context,parent,expected,{...child,status:'in_progress'},
    {inputs:{observer:JSON.stringify(context),source_revision:context.source_revision}},context);
  const dir=fs.mkdtempSync(path.join(os.tmpdir(),'harn-canary-budget-'));
  process.on('exit',()=>fs.rmSync(dir,{recursive:true}));
  const archives=[[observerDeclarationName(context),'consumer-observer.json',context,parent],
    ['consumer-observer-'+context.dispatch_nonce,'consumer-observer-'+context.dispatch_nonce+'.json',receipt,child]]
    .map(([name,filename,value,run],index)=>{
      const file=path.join(dir,filename),archive=path.join(dir,index+'.zip');
      fs.writeFileSync(file,JSON.stringify(value));execFileSync('zip',['-j',archive,file],{stdio:'ignore'});
      const bytes=fs.readFileSync(archive);
      return {bytes,metadata:{id:501+index,name,expired:false,size_in_bytes:bytes.length,
        digest:'sha256:'+createHash('sha256').update(bytes).digest('hex'),workflow_run:{id:run.id,head_sha:run.head_sha}}};
    });
  const startFile=path.join(process.env.STUB,'job-start');
  const start=fs.existsSync(startFile)?Number(fs.readFileSync(startFile,'utf8')):context.started_at;
  const queued=['queued','waiting','pending','requested'].includes(status);
  const job={id:301,run_id:child.id,run_attempt:1,head_sha:child.head_sha,
    created_at:iso(context.started_at),started_at:iso(queued?context.started_at:start),
    completed_at:status==='completed'?iso(now):null,status,conclusion,
    runner_id:queued?0:1,runner_name:queued?'':'fixture-worker',steps:queued?[]:[
      {status:status==='completed'?'completed':'in_progress',conclusion:null,started_at:iso(start)}]};
  globalThis.fetch=async(url,options={})=>{
    const faultFile=path.join(process.env.STUB,'budget-faults');
    if(fs.existsSync(faultFile)){
      const faults=Number(fs.readFileSync(faultFile,'utf8'));
      if(faults>0){fs.writeFileSync(faultFile,String(faults-1));throw new Error('private widget-host read timeout');}
    }
    if(options.method&&options.method!=='GET')throw new Error('unexpected-budget-write');
    const p=new URL(url).pathname;
    if(p.endsWith('/'+parent.id+'/attempts/'+parent.run_attempt))return {ok:true,json:async()=>parent};
    if(p.endsWith('/'+child.id))return {ok:true,json:async()=>child};
    if(p.endsWith('/runs'))return {ok:true,json:async()=>({total_count:1,workflow_runs:[child]})};
    if(p.endsWith('/jobs'))return {ok:true,json:async()=>({total_count:1,jobs:[job]})};
    if(p.endsWith('/artifacts')){
      const index=p.includes('/'+parent.id+'/')?0:1;
      return {ok:true,json:async()=>({total_count:1,artifacts:[archives[index].metadata]})};
    }
    if(p.endsWith('/zip'))return {ok:true,arrayBuffer:async()=>archives[p.includes('/501/')?0:1].bytes};
    throw new Error('unexpected-budget-read');
  };
}
JS
export NODE_OPTIONS="--import=$scratch/parent-fetch.mjs" CANARY_TEST_ROOT="$root"
export GH_TOKEN=fixture-consumer PARENT_READ_TOKEN=fixture-parent
export GITHUB_REPOSITORY=burin-labs/harn GITHUB_RUN_ID=900 GITHUB_RUN_ATTEMPT=1
export GITHUB_SHA=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
# A pull_request CI run would otherwise name its own head as the fixture parent's.
export GITHUB_EVENT_NAME=workflow_dispatch
unset GITHUB_EVENT_PATH
OBSERVER_CONTEXT=$(jq -nc --argjson started "$(date +%s)" '{
  schema:"harn.consumer_observer.v2",parent_repository:"burin-labs/harn",parent_run_id:"900",
  parent_run_attempt:1,parent_head_sha:"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  source_revision:"0123456789abcdef0123456789abcdef01234567",
  dispatch_nonce:"01234567-89ab-4cde-8fab-0123456789ab",started_at:$started
}')
export OBSERVER_CONTEXT

# A stand-in `gh` that answers from files, so each case sets the consumer's
# reply without a network. `--include` answers with an HTTP status line and
# headers the way gh does. Each line of $STUB/faults answers one consumer read
# instead, as `<status> [header=value ...]`, with a body that carries a private
# value no output may repeat.
mkdir -p "$scratch/bin"
cat > "$scratch/bin/gh" <<'EOF'
#!/usr/bin/env bash
include=false
if [[ "$1 $2" == "api --include" ]]; then include=true; shift 2; set -- api "$@"; fi
if [[ "$include" == true && "$2" == repos/acme/widget-host* && -s "$STUB/faults" ]]; then
  read -r status headers < "$STUB/faults"
  sed -i.bak 1d "$STUB/faults"
  printf 'HTTP/2.0 %s Fixture\r\n' "$status"
  for header in $headers; do printf '%s: %s\r\n' "${header%%=*}" "${header#*=}"; done
  printf '\r\n{"message":"fixture refusal fixture-private-value"}\n'
  echo fault >> "$STUB/fault-count"
  exit 1
fi
code=200
respond() {
  case "$1 $2" in
    "api repos/acme/widget-host") echo main ;;
    "api repos/acme/widget-host/pulls/"*) cat "$STUB/pull" ;;
    "api repos/acme/widget-host/actions/runs/"*)
      if [[ "${GH_TOKEN:-}" == fixture-expired ]]; then
        code=401
        echo '{"message":"Bad credentials fixture-private-value"}'
        return 1
      fi
      cat "$STUB/run" ;;
    "workflow run")
      if [[ -s "$STUB/dispatch-faults" ]]; then
        head -1 "$STUB/dispatch-faults"; sed -i.bak 1d "$STUB/dispatch-faults"; return 1
      fi
      echo "$*" > "$STUB/dispatched"; echo dispatch >> "$STUB/dispatch-count"; cat "$STUB/dispatch" ;;
    "api repos/acme/harn/actions/workflows/"*) [[ -f "$STUB/history" ]] || { code=404; return 1; }; cat "$STUB/history" ;;
    "api repos/acme/harn/actions/runs/"*/jobs) id=${2#repos/acme/harn/actions/runs/}; jq -r "$4" "$STUB/jobs-${id%/jobs}" ;;
    "api repos/acme/harn/git/matching-refs/heads/green") [[ -f "$STUB/green-ref" ]] || exit 1; cat "$STUB/green-ref" ;;
    *) echo "unexpected gh $*" >&2; exit 2 ;;
  esac
}
respond "$@" > "$STUB/gh-body"
rc=$?
if [[ "$include" == true ]]; then
  ((rc == 0)) || [[ "$code" != 200 ]] || code=500
  printf 'HTTP/2.0 %s Fixture\r\nX-Ratelimit-Remaining: 4000\r\n\r\n' "$code"
fi
cat "$STUB/gh-body"
exit "$rc"
EOF
chmod +x "$scratch/bin/gh"
# Retries back off without waiting in tests.
export CANARY_API_BACKOFF_SECONDS=0

canary() {
  local workspace_version=${2:-1.2.4-dev}
  PATH="$scratch/bin:$PATH" STUB="$scratch" CANARY_REPOSITORY=acme/widget-host \
    CANARY_WORKFLOW=rehearsal.yml SOURCE_REVISION=0123456789abcdef0123456789abcdef01234567 \
    WORKSPACE_VERSION="$workspace_version" CANARY_POLL_SECONDS=0 PAIRING_TEXT="${1:-}" \
    CONSUMER_PULL_REQUEST="${3:-${CONSUMER_PULL_REQUEST:-}}" \
    GITHUB_OUTPUT="$scratch/gho" \
    bash "$root/scripts/ci/consumer_canary.sh" > "$scratch/out" 2>&1
  local status=$?
  # This output is public; the consumer's name or a link to it never is.
  if grep -qE 'widget-host|github\.com' "$scratch/out"; then
    echo "canary output names the consumer:" >&2
    cat "$scratch/out" >&2
    exit 1
  fi
  return "$status"
}

# `! canary` would not stop the test under errexit, so a refusal is asserted
# explicitly, with its reason.
refuses() {
  local reason=$1
  shift
  if canary "$@"; then
    echo "expected refusal $reason, got green" >&2
    exit 1
  fi
  grep -q "reason=$reason" "$scratch/out"
}

assert_no_verdict() {
  if grep -q '^verdict=' "$scratch/gho"; then
    echo 'unmeasured observation produced a verdict' >&2
    exit 1
  fi
}

run_url=https://github.com/acme/widget-host/actions/runs/42
echo "$run_url" > "$scratch/dispatch"

# Only a completed success is green, and the output names the verdict, the link
# and the wall time.
echo "completed success" > "$scratch/run"
: > "$scratch/gho"
if ! canary; then cat "$scratch/out" >&2; exit 1; fi
grep -q "verdict=pass conclusion=success run=42 wall_seconds=" "$scratch/out"
grep -qx "verdict=pass" "$scratch/gho"
grep -q "consumer=configured secret=CONSUMER_CANARY_REPOSITORY" "$scratch/out"
grep -q -- '--ref main ' "$scratch/dispatched"
grep -q -- '-f target=v1.2.3 ' "$scratch/dispatched"
! grep -q -- 'release_producer_run_' "$scratch/dispatched"

# A GitHub API refusal is classified by its HTTP status and rate-limit headers.
# A rate limit or a server error is retried within bounds and then measures; a
# denial fails at once and names the token and repository by label; no output
# repeats the response body.
api_faults() {
  printf '%s\n' "$@" > "$scratch/faults"
  rm -f "$scratch/fault-count" "$scratch/dispatched"
}
assert_private_body_unprinted() {
  if grep -q 'fixture-private-value' "$scratch/out"; then
    echo 'an API refusal printed its response body' >&2; cat "$scratch/out" >&2; exit 1
  fi
}
api_faults '403 X-Ratelimit-Remaining=0 X-Ratelimit-Reset=1' '502' '429 Retry-After=0'
canary
grep -q 'retry=api_rate_limited read=consumer_unreadable status=403 attempt=1 wait_seconds=1' "$scratch/out"
grep -q 'retry=api_unavailable read=consumer_unreadable status=502 attempt=2' "$scratch/out"
grep -q 'retry=api_rate_limited read=consumer_unreadable status=429 attempt=3 wait_seconds=0' "$scratch/out"
grep -q 'verdict=pass' "$scratch/out"
[[ $(wc -l < "$scratch/fault-count") -eq 3 && -e "$scratch/dispatched" ]]
assert_private_body_unprinted
for denial in 401 403 404; do
  api_faults "$denial"
  CANARY_TOKEN_LABEL=consumer-dispatch-app-token refuses api_access_denied
  grep -q "read=consumer_unreadable status=$denial token=consumer-dispatch-app-token repository=secret:CONSUMER_CANARY_REPOSITORY attempts=1" "$scratch/out"
  [[ $(wc -l < "$scratch/fault-count") -eq 1 && ! -e "$scratch/dispatched" ]]
  assert_private_body_unprinted
done
# A rate-limited dispatch was not acted on and is retried; any other refusal
# may have dispatched and is final.
printf '%s\n' 'HTTP 403: API rate limit exceeded for installation fixture-private-value' \
  'HTTP 429: too many requests fixture-private-value' > "$scratch/dispatch-faults"
rm -f "$scratch/dispatched"
canary
grep -q 'retry=api_rate_limited read=dispatch status=403 attempt=1' "$scratch/out"
grep -q 'retry=api_rate_limited read=dispatch status=429 attempt=2' "$scratch/out"
grep -q 'verdict=pass' "$scratch/out"
assert_private_body_unprinted
for refusal in 'HTTP 422: Unexpected inputs fixture-private-value' 'HTTP 502: Bad gateway fixture-private-value' \
    'HTTP 403: Resource not accessible by integration fixture-private-value'; do
  printf '%s\n' "$refusal" "$refusal" > "$scratch/dispatch-faults"
  rm -f "$scratch/dispatched"
  refuses dispatch_refused
  grep -q "status=${refusal:5:3} token=GH_TOKEN repository=secret:CONSUMER_CANARY_REPOSITORY attempts=1" "$scratch/out"
  [[ $(wc -l < "$scratch/dispatch-faults") -eq 1 && ! -e "$scratch/dispatched" ]]
  assert_private_body_unprinted
done
: > "$scratch/dispatch-faults"
api_faults 503 503 503 503
refuses api_unavailable
grep -q 'read=consumer_unreadable status=503 token=GH_TOKEN repository=secret:CONSUMER_CANARY_REPOSITORY attempts=4' "$scratch/out"
[[ ! -e "$scratch/dispatched" ]]
# A limit that resets beyond the wait bound is not slept on.
api_faults '403 X-Ratelimit-Remaining=0 X-Ratelimit-Reset=4102444800'
refuses api_rate_limited
grep -q 'status=403 token=GH_TOKEN repository=secret:CONSUMER_CANARY_REPOSITORY attempts=1 ratelimit_remaining=0' "$scratch/out"
assert_private_body_unprinted
: > "$scratch/faults"

# Archive mode carries the current observer's exact producer identity. Invalid
# or mixed modes fail before a downstream run can be dispatched.
archive_source=0123456789abcdef0123456789abcdef01234567
archive_context=$(jq -c --arg source "$archive_source" '.parent_head_sha=$source' <<< "$OBSERVER_CONTEXT")
archive_canary() {
  OBSERVER_CONTEXT="$archive_context" GITHUB_SHA="$archive_source" GITHUB_REF=refs/heads/main \
    CANARY_RELEASE_PRODUCER_RUN_ID="$1" CANARY_RELEASE_PRODUCER_RUN_ATTEMPT="$2" \
    canary "" 1.2.3
}
archive_canary 900 1
grep -q -- '-f release_producer_run_id=900 -f release_producer_run_attempt=1' "$scratch/dispatched"
grep -q -- "-f source_revision=$archive_source " "$scratch/dispatched"
for identity in '900:' ':1' '901:1' '900:2' '900:nope'; do
  IFS=: read -r producer attempt <<< "$identity"
  rm -f "$scratch/dispatched"
  if archive_canary "${producer:-}" "${attempt:-}"; then
    echo 'invalid producer identity dispatched a consumer' >&2; exit 1
  fi
  grep -q 'reason=release_producer_identity_invalid' "$scratch/out"
  [[ ! -e "$scratch/dispatched" ]]
done
for mixed in CANARY_PULL_REQUEST_CLI_RUN CANARY_PULL_REQUEST_BASE REHEARSAL_PROMOTION_RUN_ID; do
  export "$mixed=123"
  rm -f "$scratch/dispatched"
  if archive_canary 900 1; then
    echo 'mixed archive and rehearsal mode dispatched a consumer' >&2; exit 1
  fi
  grep -q 'reason=release_producer_identity_invalid' "$scratch/out"
  [[ ! -e "$scratch/dispatched" ]]
  unset "$mixed"
done
rm -f "$scratch/dispatched"
if OBSERVER_CONTEXT="$OBSERVER_CONTEXT" GITHUB_REF=refs/heads/main \
    CANARY_RELEASE_PRODUCER_RUN_ID=900 CANARY_RELEASE_PRODUCER_RUN_ATTEMPT=1 canary; then
  echo 'foreign source accepted the current producer identity' >&2; exit 1
fi
grep -q 'reason=release_producer_identity_invalid' "$scratch/out"
[[ ! -e "$scratch/dispatched" ]]
pin=cccccccccccccccccccccccccccccccccccccccc
CANARY_PULL_REQUEST_CLI_RUN=123 CANARY_PULL_REQUEST_BASE=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb CANARY_CONSUMER_REVISION=$pin canary
grep -q -- "-f harn_cli_run=123 -f harn_base=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb -f consumer_revision=$pin" "$scratch/dispatched"
! grep -q -- 'release_producer_run_' "$scratch/dispatched"
! grep -q -- 'harn_cli_artifact' "$scratch/dispatched"
# A pull request check never rehearses whatever the consumer's branch holds.
rm -f "$scratch/dispatched"
CANARY_PULL_REQUEST_CLI_RUN=123 CANARY_PULL_REQUEST_BASE=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb \
  refuses consumer_revision_invalid
[[ ! -e "$scratch/dispatched" ]]
# The base CLI is rehearsed only at the base itself, on the same pinned consumer.
pr_base=0123456789abcdef0123456789abcdef01234567
CANARY_PULL_REQUEST_CLI_RUN=123 CANARY_PULL_REQUEST_BASE=$pr_base CANARY_CONSUMER_REVISION=$pin \
  CANARY_PULL_REQUEST_CLI_ARTIFACT=harn-base-cli.tar.zst canary
grep -q -- "-f harn_base=$pr_base -f consumer_revision=$pin -f harn_cli_artifact=harn-base-cli.tar.zst" "$scratch/dispatched"
for mismatch in "harn-base-cli.tar.zst bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" "harn-cli.tar.zst $pr_base"; do
  read -r artifact base <<< "$mismatch"
  rm -f "$scratch/dispatched"
  CANARY_PULL_REQUEST_CLI_RUN=123 CANARY_PULL_REQUEST_BASE=$base CANARY_CONSUMER_REVISION=$pin \
    CANARY_PULL_REQUEST_CLI_ARTIFACT=$artifact refuses pull_request_identity_invalid
  [[ ! -e "$scratch/dispatched" ]]
done

# The current development identity maps to the latest published tag, which is
# the version the consumer accepts as its repin floor.
echo "completed success" > "$scratch/run"
canary "" 0.10.153-dev
grep -q -- '-f target=v0.10.152 ' "$scratch/dispatched"

# A prerelease that is not the workspace's canonical development identity has
# no published target and must fail before contacting or dispatching to the
# consumer.
rm -f "$scratch/dispatched"
refuses workspace_version_unpublished "" 1.2.4-rc.1
[[ ! -e "$scratch/dispatched" ]]

# Every other terminal state is red by name, and is still a settled verdict.
for conclusion in failure cancelled timed_out; do
  echo "completed $conclusion" > "$scratch/run"
  : > "$scratch/gho"
  refuses consumer_rehearsal_failed
  grep -qx "verdict=fail" "$scratch/gho"
done
echo 'completed none' > "$scratch/run"
: > "$scratch/gho"
refuses job_timing_unmeasured
assert_no_verdict

# A run that never concludes is unmeasured: red by name, and no verdict.
echo "in_progress none" > "$scratch/run"
: > "$scratch/gho"
if PATH="$scratch/bin:$PATH" STUB="$scratch" CANARY_REPOSITORY=acme/widget-host \
  CANARY_WORKFLOW=rehearsal.yml SOURCE_REVISION=0123456789abcdef0123456789abcdef01234567 \
  WORKSPACE_VERSION=1.2.4-dev CANARY_POLL_SECONDS=0 CANARY_DEADLINE_SECONDS=-1 \
  GITHUB_OUTPUT="$scratch/gho" \
  bash "$root/scripts/ci/consumer_canary.sh" > "$scratch/out" 2>&1; then
  echo "an unconcluded consumer run reported green" >&2
  exit 1
fi
grep -q "reason=aggregate_deadline" "$scratch/out"
if grep -q '^verdict=' "$scratch/gho"; then
  echo "an unmeasured run wrote a verdict" >&2
  exit 1
fi
grep -q 'failure_receipt=recorded queue_depth=1 pending=1' "$scratch/out"
receipt=$(sed -n 's/^failure_receipt=//p' "$scratch/gho")
jq -e '.run_id == "42" and .queue.depth == 1 and .queue.pending_runs == [{run_id:"99",run_attempt:1}]' "$receipt" > /dev/null

# An unset repository secret arrives as a bare owner and fails by name.
if PATH="$scratch/bin:$PATH" STUB="$scratch" CANARY_REPOSITORY=acme/ \
  CANARY_WORKFLOW=rehearsal.yml SOURCE_REVISION=0123456789abcdef0123456789abcdef01234567 \
  WORKSPACE_VERSION=1.2.4-dev CANARY_POLL_SECONDS=0 \
  bash "$root/scripts/ci/consumer_canary.sh" > "$scratch/out" 2>&1; then
  echo "an unset consumer repository reported green" >&2
  exit 1
fi
grep -q "reason=consumer_repository_unset secret=CONSUMER_CANARY_REPOSITORY" "$scratch/out"

# A dispatch that names no run is not a pass.
: > "$scratch/dispatch"
refuses dispatch_returned_no_run
echo "$run_url" > "$scratch/dispatch"

# A pairing rehearses the paired pull request's branch.
echo "completed success" > "$scratch/run"
echo "open acme/widget-host fix-flag" > "$scratch/pull"
canary $'Body\n\nPairs-with: consumer#7'
grep -q -- '--ref fix-flag ' "$scratch/dispatched"
grep -q "ref=pull-7" "$scratch/out"

# Recovery selects the same registered open pull without changing the source
# commit's metadata. Matching metadata agrees; conflicting metadata refuses.
canary '' 1.2.4-dev 7
grep -q -- '--ref fix-flag ' "$scratch/dispatched"
grep -q "ref=pull-7" "$scratch/out"
canary 'Pairs-with: consumer#7' 1.2.4-dev 7
CONSUMER_PULL_REQUEST=7 refuses pairing_ambiguous 'Pairs-with: consumer#8'
CONSUMER_PULL_REQUEST=0 refuses pairing_unrecognized
CONSUMER_PULL_REQUEST=01 refuses pairing_unrecognized
echo "closed acme/widget-host fix-flag" > "$scratch/pull"
CONSUMER_PULL_REQUEST=7 refuses paired_pull_not_open

# A pairing it cannot honour fails by name instead of rehearsing the default.
echo "open someone/fork fix-flag" > "$scratch/pull"
CONSUMER_PULL_REQUEST=7 refuses paired_pull_from_fork
refuses paired_pull_from_fork 'Pairs-with: consumer#7'
refuses pairing_ambiguous $'Pairs-with: consumer#7\nPairs-with: consumer#8'
refuses pairing_unrecognized 'Pairs-with: other#7'
refuses pairing_unrecognized 'Pairs-with: consumer#0'
echo "open acme/widget-host fix-flag" > "$scratch/pull"
refuses pairing_unrecognized $'Pairs-with: consumer#7\nPairs-with: other#7'

# The pure parser must fail closed when its declared runtime is unavailable.
# This search directory has only the parser's shell bootstrap utilities.
mkdir "$scratch/no-node"
ln -s /usr/bin/env "$scratch/no-node/env"
ln -s /usr/bin/dirname "$scratch/no-node/dirname"
cat > "$scratch/read-pairing.sh" <<'EOF'
source "$PAIRING_OWNER"
canary_read_pairing "$PAIRING_TEXT"
printf '%s' "$CANARY_PAIRED"
EOF
env -i PATH="$PATH" PAIRING_OWNER="$root/scripts/ci/consumer_canary.sh" \
  PAIRING_TEXT='Pairs-with: consumer#7' /bin/bash "$scratch/read-pairing.sh" > "$scratch/pairing-only"
[[ $(cat "$scratch/pairing-only") == 7 ]]
pairing_status=0
env -i PATH="$scratch/no-node" PAIRING_OWNER="$root/scripts/ci/consumer_canary.sh" \
  PAIRING_TEXT='Pairs-with: consumer#7' /bin/bash "$scratch/read-pairing.sh" \
  > "$scratch/pairing-only" 2> "$scratch/out" || pairing_status=$?
[[ $pairing_status == 1 && ! -s "$scratch/pairing-only" && -s "$scratch/out" ]]

# Negative control for the identity guard: a line naming the consumer is
# refused instead of printed.
if (
  source "$root/scripts/ci/consumer_canary.sh"
  CANARY_SECRET_NAME=widget-host
  canary_say "run=https://github.com/acme/widget-host/actions/runs/42"
) > "$scratch/out" 2>&1; then
  echo "identity guard printed the consumer's name" >&2
  exit 1
fi
grep -q "reason=identity_in_output" "$scratch/out"
if grep -q widget-host "$scratch/out"; then
  echo "identity guard echoed the consumer's name" >&2
  exit 1
fi

# The decide step skips a scheduled run only when the rehearsed revision has
# not moved since the last settled verdict, and names what it compared.
main_sha=0123456789abcdef0123456789abcdef01234567
old_sha=89abcdef0123456789abcdef0123456789abcdef
green_sha=fedcba9876543210fedcba9876543210fedcba98
decide() {
  : > "$scratch/gho"
  PATH="$scratch/bin:$PATH" STUB="$scratch" EVENT_NAME=$1 SOURCE_REVISION=${2-$main_sha} \
    GITHUB_REPOSITORY=acme/harn CURRENT_RUN_ID=900 GITHUB_OUTPUT="$scratch/gho" \
    bash "$root/scripts/ci/consumer_canary.sh" --decide > "$scratch/out" 2>&1
}
# settled <run> unsettled|unnamed|<sha>: the run's jobs as the API returns them.
settled() {
  case "$2" in
    unsettled) jq -n '{jobs: [{name: "Settled verdict", conclusion: "skipped", steps: []}]}' ;;
    unnamed) jq -n '{jobs: [{name: "Settled verdict", conclusion: "success",
      steps: [{name: "Record the settled verdict"}]}]}' ;;
    *) jq -n --arg sha "$2" '{jobs: [{name: "Consumer canary", conclusion: "success", steps: []},
      {name: "Settled verdict", conclusion: "success",
      steps: [{name: "Set up job"}, {name: ("Record the settled verdict at " + $sha)}]}]}' ;;
  esac > "$scratch/jobs-$1"
}
# Newest first: this run, an unsettled run at main, then a settled one.
printf '900 %s\n901 %s\n902 %s\n' "$main_sha" "$main_sha" "$old_sha" > "$scratch/history"
settled 901 unsettled
settled 902 unnamed
decide schedule
grep -qx "run=true" "$scratch/gho"
grep -qx "source_revision=$main_sha" "$scratch/gho"
grep -q "run reason=main_moved main=$main_sha last_settled=$old_sha last_settled_run=902" "$scratch/out"

settled 901 unnamed
decide schedule
grep -qx "run=false" "$scratch/gho"
grep -q "skipped reason=main_unchanged main=$main_sha last_settled=$main_sha last_settled_run=901" "$scratch/out"

# Without an explicit revision the canary rehearses green, and compares the
# revision the settled step names rather than the run's own head.
jq -n --arg sha "$green_sha" '[{ref: "refs/heads/green-x", object: {sha: "'"$old_sha"'"}},
  {ref: "refs/heads/green", object: {sha: $sha}}]' > "$scratch/green-ref"
settled 901 "$green_sha"
decide schedule ''
grep -qx "source_revision=$green_sha" "$scratch/gho"
grep -qx "run=false" "$scratch/gho"
grep -q "skipped reason=main_unchanged main=$green_sha last_settled=$green_sha last_settled_run=901" "$scratch/out"
settled 901 "$old_sha"
decide schedule ''
grep -qx "run=true" "$scratch/gho"
grep -q "run reason=main_moved main=$green_sha last_settled=$old_sha last_settled_run=901" "$scratch/out"
# An absent or unreadable green is a named failure, never main's head.
echo '[]' > "$scratch/green-ref"
if decide schedule ''; then echo "absent green decided anyway" >&2; exit 1; fi
grep -q "reason=green_absent" "$scratch/out"
rm "$scratch/green-ref"
if decide workflow_dispatch ''; then echo "unreadable green decided anyway" >&2; exit 1; fi
grep -q "reason=green_unreadable" "$scratch/out"
[[ ! -s "$scratch/gho" ]] || { echo "unreadable green wrote a decision" >&2; exit 1; }
settled 901 unnamed

# A dispatch always rehearses, even at an already settled commit.
decide workflow_dispatch
grep -qx "run=true" "$scratch/gho"
grep -q "run reason=explicit_workflow_dispatch main=$main_sha" "$scratch/out"

settled 901 unsettled
settled 902 unsettled
decide schedule
grep -qx "run=true" "$scratch/gho"
grep -q "run reason=no_settled_verdict main=$main_sha" "$scratch/out"

# Unreadable history is a named failure, never a skip.
rm "$scratch/history"
if decide schedule; then
  echo "unreadable history decided anyway" >&2
  exit 1
fi
grep -q "reason=api_access_denied read=settled_history_unreadable status=404 token=GH_TOKEN repository=acme/harn attempts=1" "$scratch/out"
if [[ -s "$scratch/gho" ]]; then
  echo "unreadable history wrote a decision" >&2
  exit 1
fi

# Renewed windows observe the same child; they never dispatch it again or
# restart its aggregate clock. The fake expiry is a read-path counterexample.
cat > "$scratch/bin/date" <<'EOF'
#!/usr/bin/env bash
if [[ "$*" == +%s ]]; then cat "$STUB/now"; else /bin/date "$@"; fi
EOF
chmod +x "$scratch/bin/date"
cat > "$scratch/bin/sleep" <<'EOF'
#!/usr/bin/env bash
read -r now < "$STUB/now"
printf '%s\n' "$((now + $1))" > "$STUB/now"
if (( $1 > 0 )) && [[ -f "$STUB/complete-after-read-wait" ]]; then
  echo 'completed success' > "$STUB/run"
fi
EOF
chmod +x "$scratch/bin/sleep"
echo 1000 > "$scratch/now"
OBSERVER_CONTEXT=$(jq -c '.started_at=1000' <<< "$OBSERVER_CONTEXT")
echo "$run_url" > "$scratch/dispatch"
rm -f "$scratch/dispatch-count"
: > "$scratch/gho"
PATH="$scratch/bin:$PATH" STUB="$scratch" CANARY_REPOSITORY=acme/widget-host \
  CANARY_WORKFLOW=rehearsal.yml SOURCE_REVISION="$main_sha" \
  WORKSPACE_VERSION=1.2.4-dev GITHUB_OUTPUT="$scratch/gho" \
  bash "$root/scripts/ci/consumer_canary.sh" --dispatch > "$scratch/out" 2>&1
grep -qx run_id=42 "$scratch/gho"
grep -qx started_at=1000 "$scratch/gho"
grep -qx 'CONSUMER_CANARY dispatched run=42 ref=default started_at=1000' "$scratch/out"

observe() {
  : > "$scratch/gho"
  PATH="$scratch/bin:$PATH" STUB="$scratch" CANARY_REPOSITORY=acme/widget-host \
    CANARY_WORKFLOW=rehearsal.yml CANARY_RUN_ID=42 CANARY_STARTED_AT=1000 CANARY_WINDOW_SECONDS="${CANARY_TEST_WINDOW_SECONDS:-0}" \
    CANARY_POLL_SECONDS=0 GH_TOKEN="$1" GITHUB_OUTPUT="$scratch/gho" \
    bash "$root/scripts/ci/consumer_canary.sh" --observe > "$scratch/out" 2>&1
}
echo 'in_progress none' > "$scratch/run"
observe fixture-fresh
grep -q 'pending run=42' "$scratch/out"
[[ ! -s "$scratch/gho" ]]

# Four refused status reads retain the original child, back off, and reach its
# terminal success without a second dispatch or resetting the deadline.
printf '503\n503\n503\n503\n' > "$scratch/faults"
touch "$scratch/complete-after-read-wait"
CANARY_CHECKPOINT_FILE="$scratch/checkpoint.json" CANARY_TEST_WINDOW_SECONDS=60 observe fixture-fresh
grep -q 'retry=api_unavailable run=42 wait_seconds=5 next_action=retry_same_run' "$scratch/out"
grep -qx verdict=pass "$scratch/gho"
jq -e '.schema == "harn.consumer_observer.checkpoint.v1" and .state == "pass"
  and .run_id == "42" and .started_at == 1000 and .deadline_at == 10900
  and .timing.owned == 1 and .next_action == "none"' "$scratch/checkpoint.json" >/dev/null
[[ $(wc -l < "$scratch/dispatch-count") -eq 1 ]]

# The authenticated Node reader can lose transport too. Its closed retry
# classification follows the same bounded shell recovery without leaking URLs.
echo 1000 > "$scratch/now"
echo 'in_progress none' > "$scratch/run"
echo 4 > "$scratch/budget-faults"
CANARY_TEST_WINDOW_SECONDS=60 observe fixture-fresh
grep -q 'retry=github_read_unavailable run=42 wait_seconds=5' "$scratch/out"
grep -qx verdict=pass "$scratch/gho"
if grep -qE 'widget-host|private|https://' "$scratch/out"; then
  echo 'transport failure leaked private input' >&2; exit 1
fi
rm "$scratch/complete-after-read-wait"

# An exhausted read window renews the same run with an explicit unknown census.
echo 1000 > "$scratch/now"
echo 'in_progress none' > "$scratch/run"
printf '503\n503\n503\n503\n' > "$scratch/faults"
CANARY_CHECKPOINT_FILE="$scratch/checkpoint.json" observe fixture-fresh
jq -e '.state == "pending" and .next_action == "renew_same_run"
  and .timing == null and .run_id == "42" and .deadline_at == 10900' "$scratch/checkpoint.json" >/dev/null
assert_no_verdict

# Retry-After survives authority renewal through the checkpoint. A new window
# waits on the same run without re-reading a still-limited API or resetting time.
printf '429 Retry-After=3600\n' > "$scratch/faults"
CANARY_CHECKPOINT_FILE="$scratch/checkpoint.json" observe fixture-fresh
jq -e '.read_not_before == 4600 and .next_action == "renew_same_run"' "$scratch/checkpoint.json" >/dev/null
faults_before=$(wc -l < "$scratch/fault-count")
CANARY_CHECKPOINT_FILE="$scratch/checkpoint.json" CANARY_TEST_WINDOW_SECONDS=60 observe fixture-fresh
[[ $(wc -l < "$scratch/fault-count") -eq "$faults_before" ]]
jq -e '.observed_at == 1060 and .read_not_before == 4600
  and .started_at == 1000 and .deadline_at == 10900 and .timing == null' "$scratch/checkpoint.json" >/dev/null
rm "$scratch/checkpoint.json"
echo 1000 > "$scratch/now"

# A paired pull closing while its observer is live retires the parent; its
# authenticated completion listener owns cancellation of the exact child.
echo 'closed acme/widget-host fix-flag' > "$scratch/pull"
if CANARY_PAIRED_PULL=7 observe fixture-fresh; then
  echo 'closed pairing kept a pending rehearsal alive' >&2; exit 1
fi
grep -q 'reason=paired_pull_no_longer_needed' "$scratch/out"
assert_no_verdict
echo 'open acme/widget-host fix-flag' > "$scratch/pull"
CANARY_PAIRED_PULL=7 observe fixture-fresh
grep -q 'pending run=42' "$scratch/out"

# The original credential is dead after an hour. A redacted immediate refusal
# carries no terminal verdict, even if the child is known to have completed.
echo 4900 > "$scratch/now"
echo 'completed cancelled' > "$scratch/run"
if observe fixture-expired; then
  echo 'expired read authority reported success' >&2; exit 1
fi
grep -q 'reason=api_access_denied read=consumer_read_refused status=401 token=GH_TOKEN repository=secret:CONSUMER_CANARY_REPOSITORY attempts=1 ratelimit_remaining=4000 run=42 verdict=unmeasured' "$scratch/out"
if grep -qE 'fixture-private-value|widget-host|github\.com' "$scratch/out"; then
  echo 'authorization refusal exposed private input' >&2; exit 1
fi
assert_no_verdict

# Renewing authority reads that exact terminal child and preserves its failure.
if observe fixture-fresh; then
  echo 'renewed authority turned a cancelled child green' >&2; exit 1
fi
grep -q 'verdict=fail conclusion=cancelled run=42 wall_seconds=3900' "$scratch/out"
grep -qx verdict=fail "$scratch/gho"

echo 6400 > "$scratch/now"
echo 'completed success' > "$scratch/run"
observe fixture-fresh
grep -q 'verdict=pass conclusion=success run=42 wall_seconds=5400' "$scratch/out"
grep -qx verdict=pass "$scratch/gho"

# A renewal cannot reset a positively reached job's 120-minute execution clock.
echo 8200 > "$scratch/now"
if observe fixture-fresh; then
  echo 'a new window reset the aggregate deadline' >&2; exit 1
fi
grep -q 'reason=execution_deadline run=42 verdict=unmeasured wall_seconds=7200' "$scratch/out"
assert_no_verdict
[[ $(wc -l < "$scratch/dispatch-count") -eq 1 ]]

# Replaying thirty minutes of initial queue preserves the execution allowance.
echo 2800 > "$scratch/job-start"
echo 'in_progress none' > "$scratch/run"
observe fixture-fresh
grep -q 'pending run=42 status=in_progress wall_seconds=7200' "$scratch/out"
assert_no_verdict
echo 10000 > "$scratch/now"
if observe fixture-fresh; then echo 'execution budget reset across renewal' >&2; exit 1; fi
grep -q 'reason=execution_deadline run=42 verdict=unmeasured wall_seconds=9000' "$scratch/out"
echo 10900 > "$scratch/now"
if observe fixture-fresh; then echo 'aggregate wall budget was not bounded' >&2; exit 1; fi
grep -q 'reason=aggregate_deadline run=42 verdict=unmeasured wall_seconds=9900' "$scratch/out"
[[ $(wc -l < "$scratch/dispatch-count") -eq 1 ]]

# The executable windows need fresh read authority, not just a longer timer.
# Parse the actual workflow/action contract, including historical-source
# recovery, so it cannot silently return to executing the old observer.
# Use the same Harn YAML reader as the owning CI policy. Cold audit workers
# have the shared Harn binary and Node, but intentionally no npm install.
HARN_BIN_NO_BUILD=1 "$root/scripts/harn_bin.sh" -- run -e '
import { read_json, read_yaml } from "std/fs"
fn main(harness: Harness) {
  harness.stdio.println(json_stringify({
    workflow: read_yaml(harness.fs, ".github/workflows/consumer-canary.yml", nil),
    action: read_yaml(harness.fs, ".github/actions/observe-consumer/action.yml", nil),
    policy: read_json(harness.fs, "scripts/ci/consumer_observation_budget.json"),
    node: read_yaml(harness.fs, ".github/actions/node-cache/action.yml", nil),
    producer: read_yaml(harness.fs, ".github/workflows/build-release-binaries.yml", nil),
  }))
}' > "$scratch/observer-contract.json"
node - "$scratch/observer-contract.json" <<'JS'
const assert = require('node:assert/strict');
const fs = require('node:fs');
const { workflow, action, policy, node, producer } = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const {jobs} = workflow;
const steps = jobs.consumers.steps;
assert.equal(jobs.decide.steps[0].with.ref, '${{ github.sha }}');
assert.equal(steps[0].with.ref, '${{ github.sha }}');
assert.equal(steps[1].with.path, 'certified-consumer-source');
// The decide job resolves the rehearsed revision once; no later job re-derives it.
const resolved = '${{ needs.decide.outputs.source_revision }}';
assert.equal(jobs.decide.outputs.source_revision, '${{ steps.decide.outputs.source_revision }}');
assert.equal(jobs.decide.steps[1].env.SOURCE_REVISION, '${{ inputs.source_revision }}');
assert.equal(steps[1].with.ref, resolved);
assert.equal(jobs.settled.steps[0].name, `Record the settled verdict at ${resolved}`);
for (const job of [jobs.consumers, jobs.settled]) {
  assert(!JSON.stringify(job).includes('inputs.source_revision'));
}
const dispatch = steps.filter((step) => step.id === 'canary');
assert.equal(dispatch.length, 1);
assert(dispatch[0].run.includes('--dispatch'));
assert(dispatch[0].run.includes('certified-consumer-source/Cargo.toml'));
for (const suffix of ['run_id', 'run_attempt']) {
  const name = `release_producer_${suffix}`;
  assert.deepEqual(workflow.on.workflow_call.inputs[name], {
    description: workflow.on.workflow_call.inputs[name].description,
    required: false, default: '', type: 'string',
  });
  assert.equal(dispatch[0].env[`CANARY_${name.toUpperCase()}`], '${{ inputs.' + name + ' }}');
}
const archiveCall = producer.jobs['consumer-rehearsal'];
assert.deepEqual(archiveCall.needs, ['setup', 'build', 'collect_candidate_manifest', 'native-qualification']);
assert.equal(archiveCall.with.release_producer_run_id,
  "${{ needs.setup.outputs.build_mode == 'candidate' && github.run_id || '' }}");
assert.equal(archiveCall.with.release_producer_run_attempt,
  "${{ needs.setup.outputs.build_mode == 'candidate' && github.run_attempt || '' }}");
const admitted = (mode, build, manifest, setup='success', source='a'.repeat(40), cancelled=false,
  native={result:'success'}, event='push', purpose='release') =>
  require('node:vm').runInNewContext(archiveCall.if.replaceAll('needs.native-qualification', 'needs["native-qualification"]'), {
    cancelled: () => cancelled,
    github: {event_name: event},
    needs: {setup: {result: setup, outputs: {build_mode: mode, rehearsal_source_sha: source, candidate_purpose: purpose}},
      build: {result: build}, collect_candidate_manifest: {result: manifest},
      'native-qualification': native},
  });
assert.equal(admitted('candidate', 'success', 'success'), true);
for (const incomplete of ['failure', 'cancelled', 'skipped', 'pending', undefined]) {
  assert.equal(admitted('candidate', incomplete, 'success'), false);
  assert.equal(admitted('candidate', 'success', incomplete), false);
  assert.equal(admitted('candidate', 'success', 'success', 'success', 'a'.repeat(40), false, {result:incomplete}), false);
  // A scheduled release candidate earns native proof like any other; only a
  // non-release source candidate rehearses without it.
  assert.equal(admitted('candidate', 'success', 'success', 'success', 'a'.repeat(40), false, {result:incomplete}, 'schedule'), false);
  assert.equal(admitted('candidate', 'success', 'success', 'success', 'a'.repeat(40), false, {result:incomplete}, 'schedule', 'source'), true);
}
assert.equal(admitted('none', 'skipped', 'skipped'), true);
assert.equal(admitted('none', 'skipped', 'skipped', 'failure'), false);
assert.equal(admitted('candidate', 'success', 'success', 'success', ''), false);
assert.equal(admitted('none', 'skipped', 'skipped', 'success', 'a'.repeat(40), true), false);
const verifyNode = (candidate) => {
  const setup = candidate.filter((step) => step.id === 'pairing-node');
  assert.equal(setup.length, 1);
  assert.equal(setup[0].uses, node.runs.steps[0].uses);
  assert.equal(setup[0].with['node-version'], '22');
  assert.equal(setup[0].if, undefined);
  assert(candidate.indexOf(setup[0]) < candidate.findIndex((step) => step.id === 'canary'));
};
verifyNode(steps);
assert.throws(() => verifyNode(steps.filter((step) => step.id !== 'pairing-node')));
const windows = steps.filter((step) => step.uses === './.github/actions/observe-consumer');
assert.equal(windows.length, Math.ceil(policy.wall_seconds / policy.window_seconds));
// Pull requests observe the same child through the entire declared budget.
const minutes = (seconds) => Math.ceil((seconds + policy.cleanup_seconds) / 60);
assert.equal(jobs.consumers['timeout-minutes'],
  minutes(policy.wall_seconds));
for (const step of windows) {
  assert.equal(step.with['run-id'], '${{ steps.canary.outputs.run_id }}');
  assert.equal(step.with['started-at'], '${{ steps.canary.outputs.started_at }}');
  assert.equal(step.with['observer-context'], '${{ steps.observer.outputs.context }}');
  assert.equal(step.with['rehearsal-promotion-run-id'], '${{ inputs.rehearsal_promotion_run_id }}');
  assert.equal(step.with['single-window'], undefined);
  assert(jobs.consumers.outputs.verdict.includes(`steps.${step.id}.outputs.verdict`));
}
const interrupted = steps.find(step => step.id === 'interrupted');
assert.equal(interrupted.env.GH_TOKEN, '${{ steps.cleanup-token.outputs.token }}');
assert.equal(interrupted.env.CANARY_RUN_ID, '${{ steps.canary.outputs.run_id }}');
assert(interrupted.if.startsWith('always() && '));
for (const step of windows) {
  assert(interrupted.if.includes(`steps.${step.id}.outcome == 'cancelled'`));
  assert(interrupted.if.includes(`steps.${step.id}.outcome == 'failure'`));
  assert(interrupted.if.includes(`steps.${step.id}.outputs.failure-receipt == ''`));
}
assert(interrupted.run.endsWith('consumer-observer-failure.mjs observation_interrupted'));
const publication = steps.find(step => step.name === 'Preserve the measured observer failure');
assert(publication.if.startsWith('always() && '));
assert(publication.with.path.includes('steps.interrupted.outputs.failure_receipt'));
assert(steps.indexOf(interrupted) < steps.indexOf(publication));
const cleanupToken = steps.find(step => step.id === 'cleanup-token');
assert.equal(cleanupToken.if, "always() && steps.observer.outputs.context != ''");
const [mint, observe] = action.runs.steps;
assert.equal(mint.uses, 'actions/create-github-app-token@bcd2ba49218906704ab6c1aa796996da409d3eb1');
assert.deepEqual(Object.fromEntries(Object.entries(mint.with).filter(([key]) => key.startsWith('permission-'))), { 'permission-actions': 'read', 'permission-pull-requests': 'read' });
assert.equal(mint.with.repositories.trim(), 'harn\n${{ inputs.repository }}');
assert.equal(observe.env.GH_TOKEN, '${{ steps.token.outputs.token }}');
assert.equal(observe.env.CANARY_STARTED_AT, '${{ inputs.started-at }}');
assert.equal(observe.env.CANARY_RUN_ID, '${{ inputs.run-id }}');
assert.equal(observe.env.CANARY_WINDOW_SECONDS, String(policy.window_seconds));
assert.equal(observe.env.CANARY_DEADLINE_SECONDS,
  `\${{ inputs.observer-context != '' && '${policy.wall_seconds}' || '7200' }}`);
assert.equal(observe.env.OBSERVER_CONTEXT, '${{ inputs.observer-context }}');
assert(observe.run.includes('--observe') && !observe.run.includes('--dispatch'));
JS

echo "Consumer canary: archive admission, terminal, pairing, expiry, renewal, same-child and aggregate-deadline controls passed"
