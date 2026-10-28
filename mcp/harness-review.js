import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {createHash,randomUUID} from 'node:crypto';

const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const MAX_FILE = 4 * 1024 * 1024, MAX_TOTAL = 16 * 1024 * 1024;
// Input paths are bounded in characters, while receipts are read in bytes.
const MAX_RECEIPT = 128 * 1024;

function directory(target) {
  if(fs.existsSync(target)) {
    if(fs.realpathSync(target)!==target || !fs.statSync(target).isDirectory())throw new Error('Review directory must not contain symlinks');
    return;
  }
  directory(path.dirname(target));
  fs.mkdirSync(target,{mode:0o700});
}

function readRegular(target,max) {
  if(fs.realpathSync(path.dirname(target)) !== path.dirname(target)) throw new Error('Review path must not contain symlinks');
  const fd=fs.openSync(target,(fs.constants.O_RDONLY ?? 0) | (fs.constants.O_NOFOLLOW ?? 0) | (fs.constants.O_NONBLOCK ?? 0));
  try {
    const before=fs.fstatSync(fd);
    if(!before.isFile() || before.size>max) throw new Error('Review input must be a regular file within the byte limit');
    const bytes=Buffer.alloc(before.size);
    let count=0;
    while(count<bytes.length) {const n=fs.readSync(fd,bytes,count,bytes.length-count,null);if(!n)break;count+=n;}
    const after=fs.fstatSync(fd);
    if(count!==before.size || before.size!==after.size || before.mtimeMs!==after.mtimeMs || before.ctimeMs!==after.ctimeMs) throw new Error('Review input changed during capture; retry');
    return {bytes,mode:before.mode & 0o777};
  } finally {fs.closeSync(fd);}
}

// Generated verification has no caller commands and embeds the expected hashes.
function verifier(root,files) {
  return `const fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto'),cp=require('node:child_process');
const root=${JSON.stringify(root)},files=${JSON.stringify(files)};
for(const f of files){const p=path.join(root,f.path),s=fs.lstatSync(p);if(fs.realpathSync(p)!==p||!s.isFile()||s.size!==f.bytes||crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex')!==f.sha256)throw Error('Captured file changed: '+f.path);}
if(cp.execFileSync('git',['status','--porcelain','--untracked-files=all','--','.',':(exclude).leio-code'],{cwd:root,encoding:'utf8'}).trim())throw Error('Review snapshot was modified');
console.log(JSON.stringify({selected_files_unchanged:true,files:files.length,scope:'captured snapshot only; findings are not validated'}));`;
}

export async function captureReview(input,runProcess) {
  const git=async(args,cwd=input.repo_root)=>{
    const result=await runProcess('git',args,cwd,15000,{GIT_CONFIG_NOSYSTEM:'1',GIT_CONFIG_GLOBAL:os.devNull,GIT_CONFIG_COUNT:'0'});
    if(result.code!==0)throw new Error(result.stderr.trim() || 'Review Git setup failed');
    return result.stdout.replace(/\r?\n$/,'');
  };
  if(fs.realpathSync(await git(['rev-parse','--show-toplevel']))!==input.repo_root)throw new Error('repo_root must be the Git checkout root');
  if(!input.sources?.length)throw new Error('review requires sources: explicit repository-relative regular files');
  const selected=[],seen=new Set();let total=0;
  for(const source of input.sources) {
    const parts=source.split(/[\\/]/);
    if(path.isAbsolute(source) || parts.some(p=>!p || p==='.' || p==='..' || p==='.git' || p==='.leio-code') || source.includes('\\'))throw new Error('sources must be relative file paths without traversal or Git/harness metadata');
    if(seen.has(source))throw new Error('sources must not contain duplicate paths');seen.add(source);
    const captured=readRegular(path.join(input.repo_root,source),MAX_FILE);
    total+=captured.bytes.length;if(total>MAX_TOTAL)throw new Error('Review inputs exceed the 16 MiB total limit');
    selected.push({path:source,...captured});
  }
  const catalog=path.join(input.repo_root,'.leio-code/harness/reviews');directory(catalog);
  const id=`${Date.now()}-${randomUUID()}`,owner=path.join(catalog,id),root=path.join(owner,'snapshot');directory(root);
  const emptyHooks=path.join(owner,'empty-hooks');directory(emptyHooks);
  for(const file of selected) {
    const target=path.join(root,file.path);directory(path.dirname(target));
    fs.writeFileSync(target,file.bytes,{flag:'wx',mode:file.mode});
  }
  await git(['init','-q','--template='],root);
  await git(['config','core.hooksPath',emptyHooks],root);
  directory(path.join(root,'.git/info'));
  fs.writeFileSync(path.join(root,'.git/info/exclude'),'.leio-code/\n');
  await git(['--literal-pathspecs','add','--force','--',...selected.map(f=>f.path)],root);
  await git(['-c','user.name=LEIO Review Snapshot','-c','user.email=review@example.invalid','-c','commit.gpgsign=false','commit','-qm','Captured selected files for read-only review'],root);
  const origin={version:1,source_repo:input.repo_root,repo_root:root,captured_at:new Date().toISOString(),files:selected.map(f=>({path:f.path,sha256:hash(f.bytes),bytes:f.bytes.length})),scope:'Selected current files only; dependencies and unselected repository context are not copied. Capture is per-file, not an atomic repository snapshot.'};
  const receipt=JSON.stringify(origin,null,2);
  if(Buffer.byteLength(receipt)>MAX_RECEIPT-256)throw new Error(`Review provenance exceeds the receipt byte limit; snapshot preserved at ${root}`);
  directory(path.join(root,'.leio-code'));
  fs.writeFileSync(path.join(root,'.leio-code/review-source.json'),receipt,{flag:'wx',mode:0o600});
  return {origin,owner,verify_argv:[process.execPath,'-e',verifier(root,origin.files)]};
}

export function recordReview(capture,task) {
  const record={...capture.origin,run_id:task.runId};
  if(fs.realpathSync(capture.owner)!==capture.owner)throw new Error('Redirected review owner directory');
  const temporary=path.join(capture.owner,`record-${randomUUID()}.tmp`);
  try {
    fs.writeFileSync(temporary,JSON.stringify(record,null,2),{flag:'wx',mode:0o600});
    // Atomic publication without overwriting an existing receipt.
    fs.linkSync(temporary,path.join(capture.owner,'record.json'));
  } finally {if(fs.existsSync(temporary))fs.unlinkSync(temporary);}
}

function validatedOrigin(origin,root) {
  const relative=value=>typeof value==='string' && value.length>0 && value.length<=256 && !path.isAbsolute(value) && !value.includes('\\') && !value.includes('\0') && !value.split('/').some(p=>!p || p==='.' || p==='..' || p==='.git' || p==='.leio-code');
  const validHeader=origin?.version===1 && origin.repo_root===root && typeof origin.source_repo==='string' && path.isAbsolute(origin.source_repo) && typeof origin.captured_at==='string' && Number.isFinite(Date.parse(origin.captured_at)) && typeof origin.scope==='string';
  const validFile=file=>relative(file?.path) && typeof file.sha256==='string' && /^[a-f0-9]{64}$/.test(file.sha256) && Number.isInteger(file.bytes) && file.bytes>=0 && file.bytes<=MAX_FILE;
  if(!validHeader || !Array.isArray(origin.files) || origin.files.length<1 || origin.files.length>64 || !origin.files.every(validFile) || new Set(origin.files.map(f=>f.path)).size!==origin.files.length || origin.files.reduce((sum,f)=>sum+f.bytes,0)>MAX_TOTAL)throw new Error('Invalid review source receipt');
  return {version:1,source_repo:origin.source_repo,repo_root:root,captured_at:origin.captured_at,scope:origin.scope,files:origin.files.map(f=>({path:f.path,sha256:f.sha256,bytes:f.bytes}))};
}

const warning=(kind,target,error)=>({kind,path:target,message:String(error?.message ?? error).slice(0,512)});

function reviewOwner(root) {
  const owner=path.dirname(root),catalog=path.dirname(owner);
  if(path.basename(root)!=='snapshot' || path.basename(catalog)!=='reviews' || path.basename(path.dirname(catalog))!=='harness' || path.basename(path.dirname(path.dirname(catalog)))!=='.leio-code' || !/^[A-Za-z0-9-]+$/.test(path.basename(owner)))return null;
  return owner;
}

export function reviewOrigin(root) {
  const target=path.join(root,'.leio-code/review-source.json');
  if(!fs.existsSync(target))return null;
  return validatedOrigin(JSON.parse(readRegular(target,MAX_RECEIPT).bytes.toString('utf8')),root);
}

// Optional metadata must not interrupt a durable task's status or cancellation.
export function inspectReviewOrigin(root) {
  const target=path.join(root,'.leio-code/review-source.json');
  let error;
  try {const origin=reviewOrigin(root);if(origin)return {origin,review_warnings:[]};}catch(e){error=e;}
  const owner=reviewOwner(root);
  if(!owner && !error)return {origin:null,review_warnings:[]};
  if(owner) {
    try {
      const record=JSON.parse(readRegular(path.join(owner,'record.json'),MAX_RECEIPT).bytes.toString('utf8'));
      const origin=validatedOrigin(record,root);
      if(origin.source_repo!==path.resolve(owner,'../../../..'))throw new Error('Review source receipt has a different source root');
      return {origin,review_warnings:[warning('review_source_recovered',target,'Primary source receipt unavailable; provenance recovered from the catalog receipt. Disk was not repaired.')]};
    }catch(e){error ??= e;}
  }
  return {origin:null,review_warnings:[warning('review_source_unavailable',target,error ?? 'Review source receipt unavailable; durable task controls remain available.')]};
}

export function reviewCatalog(root) {
  const catalog=path.join(root,'.leio-code/harness/reviews');
  const result={review_runs:[],review_runs_truncated:false,review_catalog_incomplete:false,review_warnings:[]};
  if(!fs.existsSync(catalog))return result;
  let entries;
  try {
    if(fs.realpathSync(catalog)!==catalog)throw new Error('Redirected review catalog');
    entries=fs.readdirSync(catalog,{withFileTypes:true}).filter(e=>e.isDirectory()).map(e=>e.name).sort().reverse();
  }catch(error){result.review_catalog_incomplete=true;result.review_warnings.push(warning('review_catalog_unavailable',catalog,error));return result;}
  const add=(origin,id,recovered=false)=>result.review_runs.push({repo_root:origin.repo_root,run_id:id,captured_at:origin.captured_at,source_files:origin.files.length,...(recovered ? {recovered:true} : {}),next_call:{tool:'leio_code_task',arguments:{action:'status',repo_root:origin.repo_root,run_id:id},reason:'Read live status of this captured review; catalog receipts are not status snapshots.'}});
  for(const id of entries.slice(0,64)) {
    if(result.review_runs.length>=64){result.review_runs_truncated=true;break;}
    const target=path.join(catalog,id,'record.json'),snapshot=path.join(catalog,id,'snapshot');
    try {
      if(fs.existsSync(target)) {
        const record=JSON.parse(readRegular(target,MAX_RECEIPT).bytes.toString('utf8'));
        const origin=validatedOrigin(record,snapshot);
        if(origin.source_repo!==root || typeof record.run_id!=='string' || !/^[A-Za-z0-9-]+$/.test(record.run_id))throw new Error('Invalid review catalog receipt');
        add(origin,record.run_id);continue;
      }
    }catch(error){result.review_catalog_incomplete=true;result.review_warnings.push(warning('review_catalog_receipt_unavailable',target,error));}
    // Recover references from this snapshot only. Never follow redirected run directories.
    const runs=path.join(snapshot,'.leio-code/harness/runs');
    try {
      if(!fs.existsSync(runs))continue;
      if(fs.realpathSync(runs)!==runs)throw new Error('Redirected review run directory');
      const origin=reviewOrigin(snapshot);
      if(!origin || origin.source_repo!==root)throw new Error('Review source receipt unavailable for catalog recovery');
      const ids=fs.readdirSync(runs,{withFileTypes:true}).filter(e=>e.isDirectory() && /^[A-Za-z0-9-]+$/.test(e.name)).map(e=>e.name).sort().reverse();
      const count=Math.min(8,64-result.review_runs.length);
      for(const run of ids.slice(0,count))add(origin,run,true);
      if(ids.length){result.review_catalog_incomplete=true;result.review_warnings.push(warning('review_catalog_recovered',target,'Recovered run references from the captured snapshot; use status to inspect them. Disk was not repaired.'));}
      if(ids.length>count)result.review_runs_truncated=true;
    }catch(error){result.review_catalog_incomplete=true;result.review_warnings.push(warning('review_catalog_recovery_failed',runs,error));}
  }
  result.review_runs_truncated ||= entries.length>64;
  return result;
}
