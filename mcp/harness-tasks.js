import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {z} from 'zod';
import {findInstalledBinary} from './resolve-binary.js';
import {captureReview,recordReview,inspectReviewOrigin,reviewCatalog} from './harness-review.js';

export const taskInputSchema = {
  action: z.enum(['review','prepare','submit','list','status','events','result','cancel']),
  repo_root: z.string().min(1).describe('Absolute Git checkout root; use prepare to create an explicit clean worktree.'),
  run_id: z.string().max(128).regex(/^[A-Za-z0-9-]+$/).optional(),
  task: z.string().min(1).max(15000).optional(),
  worker: z.string().min(1).optional(),
  argv: z.array(z.string()).min(1).optional().describe('Explicit worker command. Omit for worker=codex to use the standard preset.'),
  verify_argv: z.array(z.string()).min(1).optional().describe('Required verification command; argv, not shell text.'),
  constraints: z.array(z.string()).optional(),
  context: z.string().max(16000).optional().describe('Caller findings and handoff context for the Codex preset; not inherited conversation.'),
  codex_binary: z.string().optional().describe('Optional absolute installed Codex executable for the preset.'),
  timeout_ms: z.number().int().min(1).max(86_400_000).optional(),
  after: z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER).optional(),
  limit: z.number().int().min(1).max(100).optional(),
  review_bytes: z.number().int().min(1024).max(32768).optional().describe('Per-artifact preview byte limit for result (default 8192).'),
  worktree_path: z.string().optional().describe('For prepare only: absolute, nonexistent destination; no automatic cleanup.'),
  base_ref: z.string().min(1).optional().describe('For prepare only: committed base, default HEAD.'),
  allow_dirty_source: z.boolean().optional().describe('For prepare only: acknowledge local source edits will be excluded.'),
  sources: z.array(z.string().min(1).max(256)).min(1).max(64).optional().describe('For review only: explicit relative regular files to capture as they exist now, including uncommitted edits. 4 MiB/file, 16 MiB total.'),
};

const DEFAULT_CONSTRAINTS = ['Do not commit or push.', 'Do not send messages.', 'Do not delegate to further agents.'];
const terminal = state => ['blocked','failed','cancelled','succeeded'].includes(state);

function executable(candidate) {
  if (!candidate || !path.isAbsolute(candidate)) return null;
  try {
    const resolved = fs.realpathSync(candidate);
    if (!fs.statSync(resolved).isFile()) return null;
    fs.accessSync(resolved,fs.constants.X_OK);
    return resolved;
  } catch { return null; }
}

export function codexArguments(input, {home = os.homedir(), find = findInstalledBinary, readOnly = false, servers = []} = {}) {
  const binary = input.codex_binary
    ? executable(input.codex_binary)
    : executable(path.join(home,'.codex/packages/standalone/current/bin/codex')) || executable(find('codex',{homeDir:home,accept:candidate=>Boolean(executable(candidate))}));
  if (!binary) throw new Error('No executable Codex found; supply codex_binary as an absolute executable path. Existing Codex login is required.');
  if(readOnly && servers.some(name=>!/^[-A-Za-z0-9_]+$/.test(name)))throw new Error('Cannot isolate an MCP server name containing dots or special characters with Codex CLI overrides; review was not started');
  const constraints = [...DEFAULT_CONSTRAINTS,...(input.constraints ?? [])];
  const prompt = [input.task, input.context ? `Caller context (verify against the checkout):\n${input.context}` : '',
    `Scope instructions:\n${constraints.map(c=>`- ${c}`).join('\n')}`,
    readOnly ? 'This is an isolated selected-file snapshot. Review only: do not edit files or install dependencies. Report findings on stdout with severity, relative path, line, evidence and reproduction; state missing context. No findings is a valid result. Verification checks snapshot integrity only, not finding correctness.' : 'Inspect repository instructions and relevant source before editing. Finish with changed files, checks run, and unresolved concerns.'].filter(Boolean).join('\n\n');
  return {argv:[binary,...(readOnly ? ['-c','approval_policy="never"','--disable','plugins','--disable','apps','--disable','hooks','--disable','multi_agent',...servers.flatMap(name=>['-c',`mcp_servers.${name}.enabled=false`])] : []),'exec',...(readOnly ? ['--ignore-rules'] : []),'--ephemeral','--json','--sandbox',readOnly ? 'read-only' : 'workspace-write','-C',input.repo_root,prompt],constraints};
}

function boundedUtf8(buffer, bytes) {
  let text=buffer.toString('utf8');
  // Decoding a partial multi-byte sequence may add replacement bytes.
  while(Buffer.byteLength(text)>bytes) text=text.slice(0,-1);
  return text;
}

// Use fixed artifact names under the expected run root, never worker-supplied paths.
export function artifactPreview(repo, id, relative, bytes, tail = false) {
  const empty = {path:null,text:'',available:false,truncated:false,error:null};
  let fd;
  try {
    if (!/^[A-Za-z0-9-]+$/.test(id)) throw new Error('invalid run ID');
    const root = fs.realpathSync(repo);
    const run = path.join(root,'.leio-code/harness/runs',id);
    const target = path.join(run,relative);
    empty.path = target;
    if (fs.realpathSync(path.dirname(target)) !== path.dirname(target)) throw new Error('redirected artifact directory');
    fd = fs.openSync(target,fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW | fs.constants.O_NONBLOCK);
    const info = fs.fstatSync(fd);
    if (!info.isFile()) throw new Error('artifact is not a regular file');
    const count = Math.min(info.size,bytes);
    const buffer = Buffer.alloc(count);
    const read = fs.readSync(fd,buffer,0,count,tail ? Math.max(0,info.size-count) : 0);
    return {...empty,text:boundedUtf8(buffer.subarray(0,read),bytes),available:true,truncated:info.size>read};
  } catch(error) {
    return {...empty,error:error.code === 'ENOENT' ? 'not available yet' : error.message};
  } finally { if(fd !== undefined) fs.closeSync(fd); }
}

function readableWorkerOutput(text) {
  return text.split('\n').map(line=>{
    try {
      const record=JSON.parse(line);
      if (record.type === 'item.completed' && record.item?.type === 'agent_message') return record.item.text;
      if (record.type === 'item.completed' && record.item?.type === 'command_execution') return `$ ${record.item.command}\n${record.item.aggregated_output ?? ''}\nexit: ${record.item.exit_code ?? 'unknown'}`;
      return line;
    } catch { return line; }
  }).join('\n');
}

function newFilePreviews(task, bytes) {
  const files=[];
  let remaining=bytes;
  const archives=task.untrackedArtifacts ?? [];
  const expected=path.join(fs.realpathSync(task.repoRoot),'.leio-code/harness/runs',task.runId,'untracked');
  for(const archive of archives.slice(0,8)) {
    if(remaining <= 0) break;
    const relative=path.relative(expected,path.resolve(archive));
    if(!relative || relative.startsWith(`..${path.sep}`) || relative === '..' || path.isAbsolute(relative)) {
      files.push({file:archive,preview:{path:null,text:'',available:false,truncated:false,error:'archive path is outside the expected run'}});
      continue;
    }
    const preview=artifactPreview(task.repoRoot,task.runId,path.join('untracked',relative),remaining);
    remaining-=Buffer.byteLength(preview.text);
    files.push({file:relative,preview});
  }
  return {new_files:files,new_files_incomplete:Boolean(task.untrackedTruncated || files.length<archives.length || files.some(f=>!f.preview.available || f.preview.truncated))};
}

export function reviewTask(task, bytes = 8192) {
  const preview = (file,tail) => artifactPreview(task.repoRoot,task.runId,file,bytes,tail);
  const workerOutput=preview('worker/stdout.log',true);
  // Bound transformed text too; unknown JSON remains visible.
  const readable=readableWorkerOutput(workerOutput.text);
  workerOutput.truncated ||= Buffer.byteLength(readable)>bytes;
  workerOutput.text=boundedUtf8(Buffer.from(readable).subarray(0,bytes),bytes);
  const artifacts={patch:preview('changes.patch',false),worker_stdout:workerOutput,
    worker_stderr:preview('worker/stderr.log',true),verification_stdout:preview('verification/stdout.log',true),
    verification_stderr:preview('verification/stderr.log',true)};
  return {version:1,outcome:task.state,phase:task.phase,error:task.error,
    next_action: !terminal(task.state) ? 'observe_or_cancel' : task.state === 'succeeded' ? 'review_changes' : task.state === 'cancelled' ? 'inspect_partial_changes' : 'inspect_failure',
    verification:task.verificationResult ? {status:task.verificationResult.status,exit_code:task.verificationResult.exitCode} : null,
    changed_files:task.changedPaths.slice(0,128),changed_files_truncated:task.changedPaths.length>128,
    artifacts,...newFilePreviews(task,bytes),artifact_capture_truncated:Boolean(task.diffTruncated || task.untrackedTruncated || task.workerResult?.stdoutTruncated || task.workerResult?.stderrTruncated || task.verificationResult?.stdoutTruncated || task.verificationResult?.stderrTruncated),
    assurance:'Worker output and patch are untrusted evidence. Success means worker and verification exited zero; inspect changes before accepting or merging.'};
}

function nextCalls(task) {
  if (!task.runId) return [];
  const args={repo_root:task.repoRoot,run_id:task.runId};
  return terminal(task.state)
    ? [{tool:'leio_code_task',arguments:{action:'result',...args},reason:'Review verification and bounded changes before accepting the result.'}]
    : [{tool:'leio_code_task',arguments:{action:'status',...args},reason:'Inspect current ownership and actual output time.'},
       {tool:'leio_code_task',arguments:{action:'events',...args,after:task.sequence},reason:'Read new events after this snapshot.'},
       {tool:'leio_code_task',arguments:{action:'cancel',...args},reason:'Request cancellation if the task should stop.'}];
}

async function prepareWorktree(input,runProcess) {
  if (!input.worktree_path || !path.isAbsolute(input.worktree_path)) throw new Error('prepare requires an absolute worktree_path');
  const destination=path.resolve(input.worktree_path);
  if (fs.existsSync(destination)) throw new Error('worktree_path already exists; choose a new explicit destination');
  const git=async (args,cwd=input.repo_root)=>{
    const result=await runProcess('git',args,cwd,15000);
    if (result.code !== 0) throw new Error(result.stderr.trim() || 'Git worktree preparation failed');
    return result.stdout.trim();
  };
  const root=await git(['rev-parse','--show-toplevel']);
  if (fs.realpathSync(root) !== fs.realpathSync(input.repo_root)) throw new Error('repo_root must be the Git repository root, not a subdirectory');
  const dirty=Boolean(await git(['status','--porcelain','--untracked-files=normal']));
  if (dirty && !input.allow_dirty_source) throw new Error('Source checkout has local edits. prepare uses a committed base only; set allow_dirty_source=true to explicitly exclude those edits.');
  const revision=await git(['rev-parse','--verify','--end-of-options',`${input.base_ref ?? 'HEAD'}^{commit}`]);
  await git(['worktree','add','--detach',destination,revision]);
  const prepared={source_repo:fs.realpathSync(root),repo_root:fs.realpathSync(destination),baseline_revision:revision,source_edits_excluded:dirty,detached:true,clean:false};
  prepared.clean=!Boolean(await git(['status','--porcelain','--untracked-files=normal'],prepared.repo_root));
  if(!prepared.clean) {
    const error='Worktree was created but is dirty after checkout (a Git hook may have changed it). Inspect the preserved worktree before submitting.';
    return {isError:true,content:[{type:'text',text:`${error} ${prepared.repo_root}`}],structuredContent:{ok:false,error,repo_root:prepared.repo_root,preparation:prepared,next_calls:[]}};
  }
  const next_calls=[{tool:'leio_code_task',arguments:{action:'submit',repo_root:prepared.repo_root,worker:'codex',task:'<bounded coding task>',verify_argv:['<verification executable>','<arguments>']},reason:'Fill in task and verification before submitting. The new worktree uses the committed base only.'}];
  return {content:[{type:'text',text:`Prepared clean detached worktree: ${prepared.repo_root}${dirty ? '; source edits excluded' : ''}`}],structuredContent:{ok:true,repo_root:prepared.repo_root,preparation:prepared,next_calls}};
}

export function createTaskHandler({runProcess}) {
  const handle = async raw => {
    const input=z.object(taskInputSchema).strict().parse(raw);
    for (const field of ['worker','argv','verify_argv']) if(input[field] !== undefined && input.action !== 'submit') throw new Error(`${field} is only valid for submit`);
    for (const field of ['task','constraints','timeout_ms','context','codex_binary']) if(input[field] !== undefined && !['submit','review'].includes(input.action)) throw new Error(`${field} is only valid for submit or review`);
    for (const field of ['after','limit']) if(input[field] !== undefined && input.action !== 'events') throw new Error(`${field} is only valid for events`);
    if (input.run_id !== undefined && ['review','prepare','submit','list'].includes(input.action)) throw new Error('run_id is only valid for an existing task');
    if (!path.isAbsolute(input.repo_root) || !fs.statSync(input.repo_root).isDirectory()) throw new Error('repo_root must be an absolute existing directory');
    input.repo_root=fs.realpathSync(input.repo_root);
    for (const field of ['worktree_path','base_ref','allow_dirty_source']) if(input[field] !== undefined && input.action !== 'prepare') throw new Error(`${field} is only valid for prepare`);
    if(input.sources !== undefined && input.action !== 'review')throw new Error('sources is only valid for review');
    if(input.review_bytes !== undefined && input.action !== 'result') throw new Error('review_bytes is only valid for result');
    if(input.action === 'prepare') return prepareWorktree(input,runProcess);
    if(input.action === 'review') {
      const task=input.task ?? 'Review the selected current files for concrete bugs and regressions.';
      // Validate Codex before creating a durable snapshot. No public argv override in review.
      const candidate=codexArguments({...input,task},{readOnly:true});
      const capture=await captureReview(input,runProcess);
      // Resolve config without starting any server. Preserve auth/provider settings,
      // then explicitly disable every resolved MCP server for this invocation.
      const prefix=preset=>preset.argv.slice(1,preset.argv.indexOf('exec'));
      const config=await runProcess(candidate.argv[0],[...prefix(candidate),'mcp','list','--json'],capture.origin.repo_root,15000);
      if(config.code!==0)throw new Error(`Cannot resolve Codex MCP configuration for isolated review. Snapshot preserved at ${capture.origin.repo_root}`);
      const servers=JSON.parse(config.stdout);
      if(!Array.isArray(servers) || servers.some(server=>typeof server.name!=='string'))throw new Error('Invalid Codex MCP configuration inventory; review was not started');
      const preset=codexArguments({...input,task,repo_root:capture.origin.repo_root},{readOnly:true,servers:servers.map(server=>server.name)});
      const isolated=await runProcess(preset.argv[0],[...prefix(preset),'mcp','list','--json'],capture.origin.repo_root,15000);
      if(isolated.code!==0)throw new Error(`Cannot verify isolated Codex configuration. Snapshot preserved at ${capture.origin.repo_root}`);
      const effective=JSON.parse(isolated.stdout);
      if(!Array.isArray(effective) || effective.some(server=>server.enabled!==false))throw new Error('Codex MCP servers are still enabled; review was not started');
      let result;
      try {
        result=await handle({action:'submit',repo_root:capture.origin.repo_root,task,worker:'codex',argv:preset.argv,verify_argv:capture.verify_argv,constraints:preset.constraints,...(input.timeout_ms ? {timeout_ms:input.timeout_ms} : {})});
      }catch(error) {
        const message=`Review submission outcome is unknown: ${String(error.message).slice(0,512)}. Inspect recorded tasks at the preserved snapshot; do not resubmit before checking.`;
        const next_calls=[{tool:'leio_code_task',arguments:{action:'list',repo_root:capture.origin.repo_root},reason:'Recover any durable run ID after the submission reply was lost.'}];
        return {isError:true,content:[{type:'text',text:message},{type:'text',text:JSON.stringify({repo_root:capture.origin.repo_root,next_calls})}],structuredContent:{ok:false,error:message,repo_root:capture.origin.repo_root,review_source:capture.origin,next_calls}};
      }
      result.structuredContent.review_source=capture.origin;
      if(result.structuredContent.ok) {
        try {recordReview(capture,result.structuredContent.task);}
        catch(error) {
          const warning={kind:'review_catalog_record_failed',path:path.join(capture.owner,'record.json'),message:`Run was submitted. Preserve its run ID and use the returned follow-up calls; do not resubmit. Catalog receipt failed: ${error.message.slice(0,512)}`};
          result.structuredContent.review_warnings=[...(result.structuredContent.review_warnings ?? []),warning];
          result.content.push({type:'text',text:JSON.stringify(warning)});
        }
      }
      result.content.push({type:'text',text:JSON.stringify({review_source:capture.origin})});
      return result;
    }
    const args=['task',input.action,'--repo',input.repo_root];
    if(input.action === 'submit') {
      if (!input.task || !input.worker || !input.verify_argv?.[0]) throw new Error('submit requires task, worker and verify_argv; omit argv only for worker=codex');
      if(input.argv && (input.context !== undefined || input.codex_binary !== undefined)) throw new Error('context/codex_binary belong to the Codex preset; omit argv or include handoff in your explicit command');
      let argv=input.argv, constraints=input.constraints ?? [];
      if(!argv) {
        if(input.worker !== 'codex') throw new Error('argv is required for workers other than codex');
        ({argv,constraints}=codexArguments(input));
      }
      if(!argv[0]) throw new Error('argv requires a nonempty executable');
      args.push('--task',input.task,'--worker',input.worker,'--argv',JSON.stringify(argv),'--verify-argv',JSON.stringify(input.verify_argv));
      for(const constraint of constraints) args.push('--constraint',constraint);
      if(input.timeout_ms) args.push('--timeout-ms',String(input.timeout_ms));
    } else if(input.action !== 'list') {
      if(!input.run_id) throw new Error(`${input.action} requires run_id`);
      args.push('--run-id',input.run_id);
    }
    if(input.action === 'events') {
      if(input.after !== undefined) args.push('--after',String(input.after));
      if(input.limit !== undefined) args.push('--limit',String(input.limit));
    }
    const override=process.env.LEIO_HARNESS_BIN;
    if(override && (!path.isAbsolute(override) || !['leio-harness','leio-harness.exe'].includes(path.basename(override)) || !executable(override))) throw new Error('LEIO_HARNESS_BIN must be an absolute leio-harness executable');
    const binary=override || findInstalledBinary(process.platform === 'win32' ? 'leio-harness.exe' : 'leio-harness');
    if(!binary) throw new Error('leio-harness is not installed; install it or set LEIO_HARNESS_BIN');
    const execution=await runProcess(binary,args,input.repo_root,15000);
    if(execution.code !== 0) return {isError:true,content:[{type:'text',text:execution.stderr.trim() || 'harness task command failed'}],structuredContent:{ok:false,exit_code:execution.code,repo_root:input.repo_root,error:execution.stderr.trim()}};
    const task=JSON.parse(execution.stdout);
    const summary=input.action === 'list' ? `${task.runs.length} recorded tasks` : input.action === 'events' ? `${task.events.length} events; next after ${task.nextAfter}` : `${task.worker}: ${task.state} (${task.phase})${task.error ? ` — ${task.error}` : ''}`;
    const structuredContent={ok:true,repo_root:input.repo_root,task,summary,next_calls:nextCalls(task)};
    const {origin,review_warnings}=inspectReviewOrigin(input.repo_root);
    if(review_warnings.length)structuredContent.review_warnings=review_warnings;
    if(origin)structuredContent.review_source=origin;
    if(input.action === 'list') {
      const {review_warnings:catalogWarnings,...catalog}=reviewCatalog(input.repo_root);
      Object.assign(structuredContent,catalog);
      if(catalogWarnings.length)structuredContent.review_warnings=[...review_warnings,...catalogWarnings];
      structuredContent.summary+=`; ${structuredContent.review_runs.length} captured review receipts${structuredContent.review_runs_truncated ? ' (catalog truncated)' : ''}`;
    }
    if(input.action === 'events') structuredContent.next_calls=[{tool:'leio_code_task',arguments:{action:'events',repo_root:input.repo_root,run_id:input.run_id,after:task.nextAfter},reason:task.hasMore ? 'Read the next event page.' : 'Observe subsequent activity using this cursor.'}];
    if(input.action === 'result') {
      structuredContent.review=reviewTask(task,input.review_bytes);
      if(origin) {
        if(task.state==='succeeded')structuredContent.review.next_action='review_findings';
        structuredContent.review.assurance='Reviewer output is untrusted evidence. Verification checks captured snapshot integrity only; independently reproduce findings before acting on them. Unselected context and dependencies were not captured.';
      }
      structuredContent.next_calls=terminal(task.state) ? [] : nextCalls(task);
    }
    return {content:[{type:'text',text:structuredContent.summary},...(structuredContent.review ? [{type:'text',text:JSON.stringify(structuredContent.review)}] : []),...(structuredContent.review_warnings?.length ? [{type:'text',text:JSON.stringify({review_warnings:structuredContent.review_warnings})}] : [])],structuredContent};
  };
  return handle;
}
