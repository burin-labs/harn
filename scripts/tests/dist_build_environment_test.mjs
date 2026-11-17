import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import {spawnSync} from 'node:child_process'
import {fileURLToPath} from 'node:url'
import test from 'node:test'

const script = fileURLToPath(new URL('../ci/prepare_dist_build.sh', import.meta.url))
const stub = `#!/usr/bin/env node
const fs = require('node:fs');
const tool = require('node:path').basename(process.argv[1]);
fs.appendFileSync(process.env.CALLS, JSON.stringify([tool, ...process.argv.slice(2)]) + '\\n');
switch (tool) {
  case 'rustc': console.log('host: ' + (process.env.NATIVE_TARGET ?? '')); break;
  case 'free': console.log(process.env.SWAP_MIB === 'missing' ? 'Mem: 15989' : 'Swap: ' + process.env.SWAP_MIB + ' 0 0'); break;
  case 'sudo': if (process.env.FAIL_SWAPON === '1' && process.argv[2] === 'swapon') process.exit(23); break;
  case 'df': console.log('fixture filesystem'); break;
}
`

function prepare(t, {host = 'Linux', target, swap = '0', nativeTarget = '', failSwapon = false} = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'harn-dist-build-'))
  t.after(() => fs.rmSync(root, {recursive: true, force: true}))
  const bin = path.join(root, 'bin')
  fs.mkdirSync(bin)
  for (const tool of ['rustc', 'free', 'sudo', 'df']) {
    fs.writeFileSync(path.join(bin, tool), stub, {mode: 0o755})
  }
  const envFile = path.join(root, 'env')
  const callsFile = path.join(root, 'calls')
  fs.writeFileSync(envFile, '')
  fs.writeFileSync(callsFile, '')
  const result = spawnSync('bash', [script, ...(target === undefined ? [] : [target])], {
    encoding: 'utf8',
    env: {
      PATH: `${bin}${path.delimiter}${process.env.PATH}`,
      RUNNER_OS: host,
      GITHUB_ENV: envFile,
      GITHUB_WORKSPACE: root,
      CALLS: callsFile,
      SWAP_MIB: swap,
      NATIVE_TARGET: nativeTarget,
      FAIL_SWAPON: failSwapon ? '1' : '0',
    },
  })
  assert.ifError(result.error)
  return {
    ...result,
    environment: fs.readFileSync(envFile, 'utf8'),
    calls: fs.readFileSync(callsFile, 'utf8').split('\n').filter(Boolean).map(JSON.parse),
  }
}

for (const {target, host, thin} of [
  {target: 'aarch64-unknown-linux-gnu', host: 'Linux', thin: true},
  {target: 'aarch64-apple-darwin', host: 'macOS', thin: true},
  {target: 'x86_64-apple-darwin', host: 'macOS', thin: true},
  {target: 'x86_64-unknown-linux-gnu', host: 'Linux', thin: false},
  {target: 'x86_64-pc-windows-msvc', host: 'Windows', thin: false},
]) {
  test(`${target} uses its distributed compiler profile`, t => {
    const result = prepare(t, {target, host, swap: '16384'})
    assert.equal(result.status, 0, result.stderr)
    assert.equal(result.environment, thin ? 'CARGO_PROFILE_DIST_LTO=thin\nCARGO_PROFILE_DIST_CODEGEN_UNITS=16\n' : '')
    assert.equal(result.calls.filter(([tool]) => tool === 'sudo').length, 0)
  })
}

test('native source build discovers the host target and provisions the release swap plan', t => {
  const result = prepare(t, {nativeTarget: 'aarch64-unknown-linux-gnu'})
  assert.equal(result.status, 0, result.stderr)
  assert.match(result.environment, /^CARGO_PROFILE_DIST_LTO=thin\n/)
  assert.deepEqual(result.calls.filter(([tool]) => tool === 'sudo'), [
    ['sudo', 'swapoff', '-a'],
    ['sudo', 'fallocate', '-l', '16G', '/mnt/harn-release.swap'],
    ['sudo', 'chmod', '600', '/mnt/harn-release.swap'],
    ['sudo', 'mkswap', '/mnt/harn-release.swap'],
    ['sudo', 'swapon', '/mnt/harn-release.swap'],
  ])
  assert.deepEqual(result.calls.find(([tool]) => tool === 'rustc'), ['rustc', '-vV'])
})

test('an unreadable swap census refuses before any host mutation', t => {
  const result = prepare(t, {target: 'x86_64-unknown-linux-gnu', swap: 'missing'})
  assert.notEqual(result.status, 0)
  assert.match(result.stderr, /swap capacity was not measured/)
  assert.equal(result.calls.filter(([tool]) => tool === 'sudo').length, 0)
})

test('failed swap activation refuses the build', t => {
  const result = prepare(t, {target: 'x86_64-unknown-linux-gnu', failSwapon: true})
  assert.equal(result.status, 23)
  assert.deepEqual(result.calls.at(-1), ['sudo', 'swapon', '/mnt/harn-release.swap'])
})

test('missing host or target refuses without provisioning', t => {
  for (const options of [{host: ''}, {nativeTarget: ''}]) {
    const result = prepare(t, options)
    assert.notEqual(result.status, 0)
    assert.equal(result.calls.filter(([tool]) => tool === 'sudo').length, 0)
  }
})
