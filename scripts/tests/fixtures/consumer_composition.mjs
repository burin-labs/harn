import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {consumerCompositionPath,consumerSourceDeclaration} from '../../lib/consumer-source.mjs';

export const repository='fixture/private-consumer';
export const sha=value=>value.repeat(40);
export const base=sha('a');
export const head=sha('2');
export const declaration=()=>consumerSourceDeclaration('Pairs-with: consumer#7\nPairs-with: consumer#8\nConsumer-composition: consumer#9');
export const commit=(value,...parents)=>({sha:sha(value),parents:parents.map(parent=>({sha:sha(parent)}))});

export function compositionFixture(){
  const manifest={schema:'harn.consumer_composition.v1',repository,base_sha:base,
    adaptations:[{pull_request:7,head_sha:sha('b')},{pull_request:8,head_sha:sha('c')}],
    repairs:[{pull_request:12,head_sha:sha('d')}]};
  const records=new Map();
  const calls=[];
  const counts=new Map();
  const components=[commit('b','a'),commit('c','a'),commit('d','a')];
  const all=[...components,commit('e','a','b'),commit('f','e','c'),commit('1','f','d'),commit('2','1')];
  const compare=(left,right,commits,status='ahead',mergeBase=left)=>{
    records.set(`/compare/${left}...${right}?per_page=100&page=1`,{
      base_commit:{sha:left},merge_base_commit:{sha:mergeBase},status,total_commits:commits.length,commits,
    });
  };
  records.set('',{full_name:repository,default_branch:'main'});
  records.set('/commits/main',{sha:base});
  compare(base,base,[],'identical');
  compare(base,head,all);
  for(const component of components){
    compare(base,component.sha,[component]);
    compare(component.sha,head,all.filter(item=>item.sha!==component.sha));
  }
  const pull=(number,revision,branch)=>({number,state:'open',merged:false,
    base:{repo:{full_name:repository},ref:'main'},head:{repo:{full_name:repository},ref:branch,sha:revision}});
  records.set('/pulls/9',pull(9,head,'combined-private-branch'));
  for(const [number,revision] of [[7,'b'],[8,'c'],[12,'d']]){
    records.set(`/pulls/${number}`,pull(number,sha(revision),`private-${number}`));
  }
  const writeManifest=value=>{
    const bytes=Buffer.from(JSON.stringify(value));
    const blob=createHash('sha1').update(`blob ${bytes.length}\0`).update(bytes).digest('hex');
    records.set(`/contents/${consumerCompositionPath}?ref=${head}`,{type:'file',path:consumerCompositionPath,
      encoding:'base64',size:bytes.length,sha:blob,content:bytes.toString('base64')});
    records.set(`/commits/${head}?per_page=100`,{...all.at(-1),files:[{filename:consumerCompositionPath,status:'added',sha:blob}]});
    return bytes;
  };
  const bytes=writeManifest(manifest);
  const fetchRecord=async(url,options)=>{
    assert.equal(new URL(url).origin,'https://api.github.com');
    assert.equal(options.headers.Authorization,'Bearer consumer-read');
    const suffix=new URL(url).pathname.replace(`/repos/${repository}`,'')+new URL(url).search;
    calls.push(suffix);
    const count=(counts.get(suffix)??0)+1;
    counts.set(suffix,count);
    const record=records.get(suffix);
    assert.notEqual(record,undefined,`unexpected authenticated read ${suffix}`);
    const value=typeof record==='function'?record(count):record;
    return new Response(JSON.stringify(value),{status:200});
  };
  return {manifest,records,calls,bytes,writeManifest,fetchRecord};
}
