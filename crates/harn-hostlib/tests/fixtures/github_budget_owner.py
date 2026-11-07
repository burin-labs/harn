"""Deterministic enrolled protocol peer; the budget ledger has its own suite."""
import base64
import json
import os
from pathlib import Path
import sys

assert sys.argv[1:] == ["--serve-native"]
assert not any(os.environ.get(key) for key in ("GH_TOKEN", "GITHUB_TOKEN", "GH_CONFIG_DIR", "GH_DEBUG"))
owner = json.loads(Path(__file__).with_name('.harn-github-budget-owner.json').read_text())
state = Path(owner['state_directory'])
try:
    Path(owner['account_config']).read_text()
except PermissionError:
    pass
else:
    raise AssertionError('token-free helper could read the private account configuration')


def reply(**fields):
    print(json.dumps({'schema': 'gh-budget.native-read.v1', **fields}), flush=True)


request = json.loads(sys.stdin.buffer.readline())
assert request['schema'] == 'gh-budget.native-read.v1'
assert request['argv'] == ['api', 'repos/example/repo/commits/pinned']
etag = (state / 'etag').read_text()
if request['phase'] == 'prepare':
    if (state / 'stall-prepare').exists():
        with (state / 'ready').open('w') as stream:
            stream.write(str(os.getpid()))
        sys.stdin.buffer.readline()
        raise AssertionError('stalled preparation must be cancelled before another message')
    reply(decision='prepared', if_none_match=etag)
else:
    assert request['phase'] == 'begin'
    if request['if_none_match'] != etag:
        reply(decision='refused', exit_code=77, request_executed=False, detail='prepared ETag changed')
    else:
        reply(decision='run', if_none_match=etag)
        started = json.loads(sys.stdin.buffer.readline())
        assert started['phase'] == 'started' and started['request_id'] == request['request_id']
        if (state / 'stall-write').exists():
            with (state / 'ready').open('w') as stream:
                stream.write(str(os.getpid()))
            with (state / 'release').open('rb') as stream:
                stream.read()
            raise AssertionError('stopped reader must be reaped by the completion deadline')
        finished = json.loads(sys.stdin.buffer.readline())
        assert finished['phase'] == 'finish' and finished['request_id'] == request['request_id']
        raw = base64.b64decode(finished['stdout'], validate=True)
        headers, body = raw.split(b'\r\n\r\n', 1)
        assert headers.startswith(b'HTTP/1.1 200')
        reply(decision='completed', exit_code=finished['exit_code'],
              stdout=base64.b64encode(body).decode(), stderr=finished['stderr'],
              request_executed=True, response_observed=True)
