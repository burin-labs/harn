import {execFileSync, spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {consumerPairingClaim} from './consumer-pairing.mjs';

const sourceRoot=fileURLToPath(new URL('../../',import.meta.url));
const sourceReader=fileURLToPath(new URL('./release_pairing_source.sh',import.meta.url));
const policy=fileURLToPath(new URL('./consumer_pairing_projection.sh',import.meta.url));
const sha=/^[0-9a-f]{40}$/;
const id=/^[1-9][0-9]*$/;
const refused=reason=>{throw new Error(`CONSUMER_OBSERVER reason=${reason}`);};

/** Original source objects and the relevant tag census own the claim, never request fields. */
export function readReleasePairingSource(sourceRevision) {
  if(!sha.test(sourceRevision??'')) refused('pairing_source_unmeasured');
  let record;
  try {
    record=JSON.parse(execFileSync('bash',[sourceReader,sourceRoot,sourceRevision],{
      encoding:'utf8',timeout:120000,maxBuffer:1048576,
      env:{PATH:process.env.PATH,HOME:process.env.HOME,GIT_TERMINAL_PROMPT:'0',GIT_NO_LAZY_FETCH:'1'},
      stdio:['ignore','pipe','pipe'],
    }));
  } catch { refused('pairing_source_unmeasured'); }
  if(record?.schema!=='harn.release.pairing_source.v1'||record.source_sha!==sourceRevision
    ||typeof record.version!=='string'||typeof record.base_tag!=='string'||!sha.test(record.base_sha??'')
    ||!Number.isSafeInteger(record.commit_count)||record.commit_count<1||typeof record.text!=='string'){
    refused('pairing_source_unmeasured');
  }
  return record;
}

/** Fresh transport records are projected to the single Bash containment owner. */
export async function qualifyRepairPairing({sourceRevision,consumerPull,testedHead,repository},{read}) {
  if(!id.test(consumerPull??'')||!sha.test(testedHead??'')
    ||typeof repository!=='string'||!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository)){
    refused('pairing_superset_unmeasured');
  }
  const source=readReleasePairingSource(sourceRevision);
  const claim=consumerPairingClaim(source.text,consumerPull);
  if(claim.pull!==consumerPull||!Array.isArray(claim.superseded)
    ||!claim.superseded.every(pull=>typeof pull==='string'&&id.test(pull))){
    refused('pairing_superset_unmeasured');
  }
  // This is an explicitly read source with no other claims, not missing v1 metadata.
  if(claim.superseded.length===0) return;
  const records={};
  for(const pull of claim.superseded){
    const endpoint=`/pulls/${pull}`;
    const head=(await read(endpoint))?.head?.sha;
    if(typeof head!=='string'||!sha.test(head)) refused('pairing_superset_unmeasured');
    records[endpoint]={head:{sha:head}};
    const comparison=`/compare/${head}...${testedHead}`;
    const response=await read(comparison);
    records[comparison]={status:response?.status,merge_base_commit:{sha:response?.merge_base_commit?.sha}};
  }
  const run=spawnSync('bash',[policy],{encoding:'utf8',timeout:30000,maxBuffer:65536,
    env:{PATH:process.env.PATH},input:JSON.stringify({schema:'harn.consumer_pairing_projection.v1',
      repository,paired:consumerPull,rehearsed:testedHead,superseded:claim.superseded.join(','),records})});
  let result;
  try { result=JSON.parse(run.stdout); } catch { refused('pairing_superset_unmeasured'); }
  if(result?.schema!=='harn.consumer_pairing_check.v1'||result.ok!==true||run.status!==0){
    refused(result?.ok===false&&['pairing_superset_missing','pairing_superset_unmeasured'].includes(result.reason)
      ?result.reason:'pairing_superset_unmeasured');
  }
  for(const pull of claim.superseded){
    const endpoint=`/pulls/${pull}`;
    if((await read(endpoint))?.head?.sha!==records[endpoint].head.sha) refused('pairing_superset_changed');
  }
}
