import assert from 'node:assert/strict';
import {test} from 'node:test';
import {consumerObservationPolicy,waitConsumerWindow} from '../lib/consumer-observer-window.mjs';

test('credential renewal preserves one aggregate budget and never turns absence into a product verdict',async()=>{
  const policy=consumerObservationPolicy();
  assert.equal(policy.max_windows,3);
  let clock=1000,calls=0;
  const pending={discovered:0,owned:0,pending:1,refused:0,execution:null,product:null};
  const instrument={now:()=>clock,pause:async seconds=>{clock+=seconds;},policy};
  const observe=async()=>{calls++;return pending;};
  const first=await waitConsumerWindow(1000,observe,instrument);
  assert.equal(first.verdict,'pending');assert.equal(first.wallSeconds,2700);
  const second=await waitConsumerWindow(1000,observe,instrument);
  assert.equal(second.verdict,'pending');assert.equal(second.wallSeconds,5400);
  const last=await waitConsumerWindow(1000,observe,instrument);
  assert.equal(last.verdict,'deadline');assert.equal(last.wallSeconds,7200);
  assert(calls>0,'a measured pending window must reach the observer');
  const expiredCalls=calls;
  assert.equal((await waitConsumerWindow(1000,observe,instrument)).verdict,'deadline');
  assert.equal(calls,expiredCalls,'an expired credential window does not issue another observation');

  clock=1000;
  const missing={...pending,discovered:1,owned:1,execution:'success'};
  const successfulExecution=await waitConsumerWindow(1000,async()=>missing,instrument);
  assert.equal(successfulExecution.verdict,'pending');assert.equal(successfulExecution.reading.product,null);
  clock=1000;
  const failedExecution=await waitConsumerWindow(1000,async()=>({...missing,pending:0,execution:'failure'}),instrument);
  assert.equal(failedExecution.verdict,'execution_failed');assert.equal(failedExecution.reading.product,null);
  for(const verdict of ['pass','fail']){
    const measured=await waitConsumerWindow(1000,async()=>({...missing,pending:0,product:{verdict}}),instrument);
    assert.equal(measured.verdict,verdict);
  }
  const refused=await waitConsumerWindow(1000,async()=>({...missing,pending:0,refused:1}),instrument);
  assert.equal(refused.verdict,'refused');
  await assert.rejects(waitConsumerWindow(1001,observe,instrument),/observation_start_invalid/);
  const late=await waitConsumerWindow(1000,async()=>{clock=8200;return {...missing,pending:0,product:{verdict:'pass'}};},instrument);
  assert.equal(late.verdict,'deadline','late positive evidence cannot reset the deadline');
});
