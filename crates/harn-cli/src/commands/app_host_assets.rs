pub(crate) fn host_document(title: &str, sandbox_origin: &str) -> String {
    let title_json = script_json(title);
    let sandbox_json = script_json(sandbox_origin);
    HOST_DOCUMENT
        .replace("__HARN_SANDBOX_ORIGIN__", &sandbox_json)
        .replace("__HARN_TITLE__", &title_json)
}

fn script_json(value: &str) -> String {
    serde_json::to_string(value)
        .expect("string serializes")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

pub(crate) fn sandbox_document() -> String {
    SANDBOX_DOCUMENT.to_string()
}

const HOST_DOCUMENT: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Harn App</title><style>
:root{color-scheme:light dark;font-family:ui-sans-serif,system-ui,sans-serif}*{box-sizing:border-box}
body{margin:0;background:#111318;color:#edf0f7}header{height:48px;display:flex;align-items:center;gap:10px;padding:0 16px;border-bottom:1px solid #2b303b;background:#171a20}
.mark{width:10px;height:10px;border-radius:50%;background:#78a8ff;box-shadow:0 0 18px #78a8ff}.name{font-weight:650}.uri{margin-left:auto;color:#939cab;font:12px ui-monospace,monospace}
main{height:calc(100vh - 48px);padding:12px}iframe{width:100%;height:100%;border:1px solid #343a47;border-radius:10px;background:white}
#status{font-size:12px;color:#939cab}
</style></head><body><header><span class="mark"></span><span class="name"></span><span id="status">starting</span><span class="uri"></span></header><main><iframe id="sandbox" sandbox="allow-scripts allow-same-origin"></iframe></main>
<script>
const title = __HARN_TITLE__; const sandboxOrigin = __HARN_SANDBOX_ORIGIN__;
const frame=document.getElementById('sandbox'), status=document.getElementById('status');
document.querySelector('.name').textContent=title; document.title=title+' — Harn App';
let descriptor=null, initialized=false, modelContext=null;
function reply(message){frame.contentWindow.postMessage(message,sandboxOrigin)}
function result(id,value){reply({jsonrpc:'2.0',id,result:value})}
function failure(id,code,message){reply({jsonrpc:'2.0',id,error:{code,message}})}
async function proxy(message){
  const response=await fetch('/rpc',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(message)});
  const body=await response.json(); if(!response.ok) throw new Error(body.error||response.statusText); reply(body);
}
async function receive(event){
  if(event.source!==frame.contentWindow||event.origin!==sandboxOrigin)return;
  const m=event.data;if(!m||m.jsonrpc!=='2.0')return;
  if(m.method==='ui/notifications/sandbox-proxy-ready'){
    descriptor=await fetch('/app').then(r=>r.json()); document.querySelector('.uri').textContent=descriptor.resourceUri;
    reply({jsonrpc:'2.0',method:'ui/notifications/sandbox-resource-ready',params:{html:descriptor.html,csp:descriptor.meta?.ui?.csp||{},permissions:descriptor.meta?.ui?.permissions||{}}}); status.textContent='ready';return;
  }
  if(m.method==='ui/initialize'){
    result(m.id,{protocolVersion:'2026-01-26',hostCapabilities:{},hostInfo:{name:'harn-app',version:'0.1.0'},hostContext:{theme:matchMedia('(prefers-color-scheme: dark)').matches?'dark':'light',displayMode:'fullscreen',availableDisplayModes:['fullscreen'],containerDimensions:{width:innerWidth,height:innerHeight-48},locale:navigator.language,timeZone:Intl.DateTimeFormat().resolvedOptions().timeZone,platform:'web'}});return;
  }
  if(m.method==='ui/notifications/initialized'){initialized=true;status.textContent='connected';return}
  if(m.method==='ui/update-model-context'){modelContext=m.params;result(m.id,{});return}
  if(m.method==='ui/notifications/size-changed')return;
  if(m.method==='notifications/message'){console.info('[app]',m.params);return}
  if(m.method&&m.method.startsWith('ui/')){if(m.id!==undefined)failure(m.id,-32601,'Host method not supported');return}
  if(m.method){try{await proxy(m)}catch(error){failure(m.id,-32000,String(error.message||error))}}
}
window.addEventListener('message',receive);
frame.src=sandboxOrigin+'/sandbox?host_origin='+encodeURIComponent(location.origin);
</script></body></html>"#;

const SANDBOX_DOCUMENT: &str = r#"<!doctype html><html><head><meta charset="utf-8"><style>html,body,#view{width:100%;height:100%;margin:0;border:0}body{overflow:hidden}</style></head><body><iframe id="view" sandbox="allow-scripts"></iframe><script>
const hostOrigin=new URLSearchParams(location.search).get('host_origin');const view=document.getElementById('view');
function send(message){parent.postMessage(message,hostOrigin)}
function csp(meta){const connect=meta.connectDomains||[],resource=meta.resourceDomains||[],frames=meta.frameDomains||[],bases=meta.baseUriDomains||[];return ["default-src 'none'","script-src 'self' 'unsafe-inline' "+resource.join(' '),"style-src 'self' 'unsafe-inline' "+resource.join(' '),"img-src 'self' data: "+resource.join(' '),"font-src 'self' "+resource.join(' '),"media-src 'self' data: "+resource.join(' '),"connect-src "+(connect.join(' ')||"'none'"),"frame-src "+(frames.join(' ')||"'none'"),"base-uri "+(bases.join(' ')||"'self'"),"object-src 'none'"].join('; ')}
function inject(html,policy){const tag='<meta http-equiv="Content-Security-Policy" content="'+policy.replaceAll('&','&amp;').replaceAll('"','&quot;')+'">';return /<head[\s>]/i.test(html)?html.replace(/<head([^>]*)>/i,'<head$1>'+tag):tag+html}
function allow(permissions){return Object.keys(permissions||{}).map(key=>key.replace(/[A-Z]/g,m=>'-'+m.toLowerCase())).join('; ')}
addEventListener('message',event=>{if(event.source===parent&&event.origin===hostOrigin){const m=event.data;if(m?.method==='ui/notifications/sandbox-resource-ready'){view.allow=allow(m.params.permissions);view.srcdoc=inject(m.params.html,csp(m.params.csp||{}));return}view.contentWindow?.postMessage(m,'*');return}if(event.source===view.contentWindow)send(event.data)});
send({jsonrpc:'2.0',method:'ui/notifications/sandbox-proxy-ready',params:{}});
</script></body></html>"#;
