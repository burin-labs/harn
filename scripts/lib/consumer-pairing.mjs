const id=/^[1-9][0-9]*$/;
const refuse=reason=>{throw new Error(`CONSUMER_OBSERVER reason=${reason}`);};

/** The pull to rehearse, and trailer-named pulls an explicit request supersedes.
 * This parser holds no credentials; its caller must prove containment. */
export function consumerPairingClaim(text, requestedPull = '') {
  const lines=String(text??'').split(/\r?\n/).filter(line=>/^\s*Pairs-with:/.test(line));
  const named=new Set();
  for(const line of lines){
    const match=/^\s*Pairs-with:\s*consumer#([1-9][0-9]*)\s*$/.exec(line);
    if(!match) refuse('consumer_pairing_invalid');
    named.add(match[1]);
  }
  const byNumber=(left,right)=>Number(left)-Number(right);
  if(requestedPull !== '') {
    if(typeof requestedPull !== 'string' || !id.test(requestedPull)) refuse('consumer_pairing_invalid');
    named.delete(requestedPull);
    return {pull:requestedPull,superseded:[...named].sort(byNumber)};
  }
  if(named.size>1){
    const error=new Error('CONSUMER_OBSERVER reason=consumer_pairing_ambiguous');
    error.pulls=[...named].sort(byNumber);
    throw error;
  }
  return {pull:named.size===1?[...named][0]:null,superseded:[]};
}

/** One pairing for callers that cannot prove containment: any second pull refuses. */
export function consumerPairing(text, requestedPull = '') {
  const claim=consumerPairingClaim(text, requestedPull);
  if(claim.superseded.length){
    const error=new Error('CONSUMER_OBSERVER reason=consumer_pairing_ambiguous');
    error.pulls=[claim.pull,...claim.superseded].sort((left,right)=>Number(left)-Number(right));
    throw error;
  }
  return claim.pull;
}
