import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {execFile,execFileSync} from 'node:child_process';
import {artifactPreview,codexArguments,reviewTask,createTaskHandler} from './harness-tasks.js';

function fixture(t) {
  const root=fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(),'harness-agent-')));
  t.after(()=>fs.rmSync(root,{recursive:true,force:true}));
  return root;
}

test('Codex preset preserves literal task/context and constrains the default handoff',t=>{
  const root=fixture(t),binary=path.join(root,'codex');
  fs.writeFileSync(binary,'#!/bin/sh\nexit 0\n',{mode:0o755});
  const task='Fix "quotes"; $(touch SHOULD_NOT_EXIST) `uname`\nsecond line';
  const result=codexArguments({repo_root:root,task,context:'caller findings',constraints:['Only change parser.rs'],codex_binary:binary});
  assert.deepEqual(result.argv.slice(0,9),[binary,'exec','--ephemeral','--json','--sandbox','workspace-write','-C',root,result.argv[8]]);
  assert.ok(result.argv[8].startsWith(task));
  assert.match(result.argv[8],/caller findings/);
  assert.match(result.argv[8],/Do not commit or push/);
  assert.match(result.argv[8],/Only change parser.rs/);
  assert.ok(!fs.existsSync(path.join(root,'SHOULD_NOT_EXIST')));
  assert.throws(()=>codexArguments({repo_root:root,task,codex_binary:'relative/codex'}),/absolute executable/);
});

test('Artifact previews bound head/tail reads and reject redirected files/directories',t=>{
  const root=fixture(t),run=path.join(root,'.leio-code/harness/runs/test-run');
  fs.mkdirSync(path.join(run,'worker'),{recursive:true});
  const file=path.join(run,'worker/stdout.log');
  fs.writeFileSync(file,'prefix-'+ 'a'.repeat(4096)+'-suffix');
  const head=artifactPreview(root,'test-run','worker/stdout.log',1024);
  assert.ok(head.available);assert.ok(head.truncated);assert.equal(Buffer.byteLength(head.text),1024);assert.ok(head.text.startsWith('prefix-'));
  assert.ok(artifactPreview(root,'test-run','worker/stdout.log',1024,true).text.endsWith('-suffix'));
  fs.writeFileSync(file,'界'.repeat(1000));
  assert.ok(Buffer.byteLength(artifactPreview(root,'test-run','worker/stdout.log',1024).text)<=1024);
  assert.ok(Buffer.byteLength(artifactPreview(root,'test-run','worker/stdout.log',1024,true).text)<=1024);
  fs.unlinkSync(file);fs.symlinkSync('/etc/hosts',file);
  assert.equal(artifactPreview(root,'test-run','worker/stdout.log',1024).available,false);
  fs.rmSync(path.join(run,'worker'),{recursive:true});fs.symlinkSync(root,path.join(run,'worker'));
  assert.match(artifactPreview(root,'test-run','worker/stdout.log',1024).error,/redirected/);
  assert.equal(artifactPreview(root,'../escape','worker/stdout.log',1024).available,false);
});

test('Review distinguishes verification failure, partial running output and acceptance evidence',t=>{
  const root=fixture(t),run=path.join(root,'.leio-code/harness/runs/test-run');
  fs.mkdirSync(path.join(run,'worker'),{recursive:true});
  fs.writeFileSync(path.join(run,'worker/stdout.log'),JSON.stringify({type:'thread.started'})+'\n'+JSON.stringify({type:'item.completed',item:{type:'agent_message',text:'I changed the parser'}})+'\n'+JSON.stringify({type:'item.completed',item:{type:'command_execution',command:'cargo test',aggregated_output:'ok',exit_code:0}})+'\n');
  const snapshot={repoRoot:root,runId:'test-run',state:'failed',phase:'completed',error:'verification failed',verificationResult:{status:'failed',exitCode:7},changedPaths:['parser.rs'],diffTruncated:false,untrackedTruncated:false};
  const review=reviewTask(snapshot,1024);
  assert.equal(review.next_action,'inspect_failure');assert.equal(review.verification.exit_code,7);
  assert.match(review.artifacts.worker_stdout.text,/I changed the parser/);
  assert.equal(review.artifacts.patch.available,false);
  assert.match(review.assurance,/untrusted/);
  assert.equal(reviewTask({...snapshot,state:'running',verificationResult:null}).next_action,'observe_or_cancel');
  assert.equal(reviewTask({...snapshot,state:'succeeded'}).next_action,'review_changes');
  assert.equal(reviewTask({...snapshot,state:'cancelled'}).next_action,'inspect_partial_changes');
});


test('Review includes captured new files within a shared budget and rejects outside archive paths',t=>{
  const root=fixture(t),run=path.join(root,'.leio-code/harness/runs/test-run');
  fs.mkdirSync(path.join(run,'untracked'),{recursive:true});
  const first=path.join(run,'untracked/new-code.js'),second=path.join(run,'untracked/report.json');
  fs.writeFileSync(first,'export const answer=42;');fs.writeFileSync(second,'x'.repeat(2048));
  const snapshot={repoRoot:root,runId:'test-run',state:'succeeded',phase:'completed',error:null,verificationResult:{status:'passed',exitCode:0},changedPaths:['new-code.js','report.json'],untrackedArtifacts:[first,second],untrackedTruncated:false};
  const review=reviewTask(snapshot,1024);
  assert.equal(review.new_files[0].file,'new-code.js');assert.match(review.new_files[0].preview.text,/answer=42/);
  assert.ok(review.new_files.reduce((sum,f)=>sum+Buffer.byteLength(f.preview.text),0)<=1024);
  assert.equal(review.new_files_incomplete,true);
  const complete=reviewTask({...snapshot,untrackedArtifacts:[first]});assert.equal(complete.new_files_incomplete,false);
  const outside=reviewTask({...snapshot,untrackedArtifacts:['/etc/hosts']});assert.equal(outside.new_files[0].preview.available,false);assert.equal(outside.new_files_incomplete,true);
  const many=reviewTask({...snapshot,untrackedArtifacts:Array(9).fill(first)});assert.equal(many.new_files.length,8);assert.equal(many.new_files_incomplete,true);
});


test('Codex discovery skips a non-executable cargo candidate and selects executable PATH candidate',t=>{
  const root=fixture(t),cargo=path.join(root,'.cargo'),valid=path.join(root,'valid-bin');
  fs.mkdirSync(path.join(cargo,'bin'),{recursive:true});fs.mkdirSync(valid);
  fs.writeFileSync(path.join(cargo,'bin/codex'),'stale non-executable',{mode:0o644});
  fs.writeFileSync(path.join(valid,'codex'),'#!/bin/sh\necho valid-codex\n',{mode:0o755});
  const priorPath=process.env.PATH,priorCargo=process.env.CARGO_HOME;
  process.env.PATH=valid;process.env.CARGO_HOME=cargo;
  t.after(()=>{if(priorPath===undefined)delete process.env.PATH;else process.env.PATH=priorPath;if(priorCargo===undefined)delete process.env.CARGO_HOME;else process.env.CARGO_HOME=priorCargo;});
  const args=codexArguments({repo_root:root,task:'fixture'},{home:root});
  assert.equal(args.argv[0],path.join(valid,'codex'));
  assert.equal(execFileSync(args.argv[0],[],{encoding:'utf8'}),'valid-codex\n');
});

test('Prepare reports a dirty checkout-hook postcondition and preserves the destination without recommending submit',async t=>{
  const parent=fixture(t),repo=path.join(parent,'source'),worktree=path.join(parent,'worktree');fs.mkdirSync(repo);
  fs.writeFileSync(path.join(repo,'code.txt'),'source\n');
  for(const args of [['init','-q'],['add','.'],['-c','user.name=Test','-c','user.email=test@example.invalid','commit','-qm','baseline']])execFileSync('git',args,{cwd:repo});
  fs.writeFileSync(path.join(repo,'.git/hooks/post-checkout'),'#!/bin/sh\nprintf "hook change\\n" >> code.txt\n',{mode:0o755});
  const runProcess=(command,args,cwd,timeout)=>new Promise(resolve=>execFile(command,args,{cwd,timeout},(error,stdout,stderr)=>resolve({code:error?.code ?? 0,stdout,stderr})));
  const result=await createTaskHandler({runProcess})({action:'prepare',repo_root:repo,worktree_path:worktree});
  assert.equal(result.isError,true);assert.equal(result.structuredContent.ok,false);
  assert.equal(result.structuredContent.preparation.clean,false);
  assert.equal(result.structuredContent.preparation.repo_root,worktree);
  assert.deepEqual(result.structuredContent.next_calls,[]);
  assert.match(fs.readFileSync(path.join(worktree,'code.txt'),'utf8'),/hook change/);
  assert.equal(fs.readFileSync(path.join(repo,'code.txt'),'utf8'),'source\n');
});
