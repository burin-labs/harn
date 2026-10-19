import assert from 'node:assert/strict';
import {test} from 'node:test';
import {assessConsumerObservation, unmeasuredConsumerTiming} from '../lib/consumer-observation-budget.mjs';

const origin = 1792362069;
const iso = seconds => new Date((origin + seconds) * 1000).toISOString();
const child = {id: 73, run_attempt: 1, head_sha: 'a'.repeat(40)};
const job = {id: 47, run_id: 73, run_attempt: 1, head_sha: child.head_sha,
  created_at: iso(74), started_at: iso(1842), completed_at: null,
  status: 'in_progress', conclusion: null, runner_id: 15, runner_name: 'owned-worker',
  steps: [{status: 'in_progress', conclusion: null, started_at: iso(1848)}]};

test('the observed thirty-minute queue does not consume a running job execution budget', () => {
  const atOldDeadline = assessConsumerObservation(origin, child, [job], origin + 7200);
  assert.equal(atOldDeadline.reason, null);
  assert.equal(atOldDeadline.queue_seconds, 1768);
  assert.equal(atOldDeadline.execution_seconds, 5358);
  assert.equal(atOldDeadline.executing_jobs, 1);
  assert.equal(assessConsumerObservation(origin, child, [job], origin + 9042).reason,
    'execution_deadline');
});

test('a queued pseudostart never grants execution time or resets the queue clock', () => {
  const queued = {...job, status: 'queued', runner_id: 0, runner_name: '',
    started_at: job.created_at, steps: []};
  const reading = assessConsumerObservation(origin, child, [queued], origin + 2774);
  assert.equal(reading.reason, 'queue_deadline');
  assert.equal(reading.execution_seconds, 0);
  assert.equal(reading.queued_jobs, 1);
  assert.equal(assessConsumerObservation(origin, child, [queued], origin + 2775).queue_seconds, 2701);
  assert.equal(assessConsumerObservation(origin, child, [{...queued, status: 'in_progress'}],
    origin + 2774).reason, 'queue_deadline');
});

test('a completed queue stops aging at its evidenced end even when the poll is delayed', () => {
  const cancelled = {...job, status: 'completed', conclusion: 'cancelled',
    runner_id: 0, runner_name: '', started_at: job.created_at, steps: [], completed_at: iso(2714)};
  const reading = assessConsumerObservation(origin, child, [cancelled], origin + 2775);
  assert.equal(reading.reason, null);
  assert.equal(reading.queue_seconds, 2640);
  assert.equal(reading.queued_jobs, 0);
  assert.equal(reading.executing_jobs, 0);
  assert.equal(assessConsumerObservation(origin, child,
    [{...cancelled, completed_at: iso(2774)}], origin + 2775).reason, 'queue_deadline');
  for (const completed_at of [null, 'bad-date', iso(73), iso(2776)]) {
    assert.throws(() => assessConsumerObservation(origin, child,
      [{...cancelled, completed_at}], origin + 2775), /job_timing_unmeasured/);
  }
});

test('job clocks are immutable across renewal and multiple independently queued jobs', () => {
  const first = {...job, id: 48, created_at: iso(10), started_at: iso(200),
    completed_at: iso(3000), status: 'completed', conclusion: 'success',
    steps: [{status: 'completed', conclusion: 'success', started_at: iso(205)}]};
  assert.equal(assessConsumerObservation(origin, child, [first, job], origin + 7200).reason, null);
  assert.equal(assessConsumerObservation(origin, child, [first, job], origin + 9042).reason,
    'execution_deadline');
  assert.equal(assessConsumerObservation(origin, child, [first, job], origin + 9900).reason,
    'aggregate_deadline');
});

test('empty, foreign, repeated, missing and malformed timing evidence refuses', () => {
  for (const jobs of [[], [job, job], [{...job, run_attempt: 2}], [{...job, run_id: 74}],
    [{...job, head_sha: 'b'.repeat(40)}], [{...job, created_at: null}],
    [{...job, created_at: 'bad-date'}], [{...job, started_at: null}],
    [{...job, steps: [{status: 'in_progress', started_at: 'bad-date'}]}],
    [{...job, started_at: iso(10)}]]) {
    assert.throws(() => assessConsumerObservation(origin, child, jobs, origin + 7200),
      /job_timing_unmeasured/);
  }
});

test('missing admission never buys execution time or a measured zero', () => {
  const pending = unmeasuredConsumerTiming(origin, origin + 2699, 0);
  assert.equal(pending.reason, null);
  assert.equal(pending.owned, 0);
  assert.equal(pending.observed_jobs, null);
  assert.equal(pending.executing_jobs, null);
  assert.equal(unmeasuredConsumerTiming(origin, origin + 2700, 0).reason, 'queue_deadline');
  assert.equal(unmeasuredConsumerTiming(origin, origin + 2701, 1).reason, 'queue_deadline');
});
