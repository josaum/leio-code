import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {execFile,execFileSync} from 'node:child_process';
import {captureReview,recordReview,reviewOrigin,inspectReviewOrigin,reviewCatalog} from './harness-review.js';
import {codexArguments,createTaskHandler} from './harness-tasks.js';

const runProcess=(command,args,cwd,timeout,env={})=>new Promise(resolve=>execFile(command,args,{cwd,timeout,env:{...process.env,...env}},(error,stdout,stderr)=>resolve({code:error?.code ?? 0,stdout,stderr})));
function fixture(t) {
  const root=fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(),'leio-review-')));
  t.after(()=>fs.rmSync(root,{recursive:true,force:true}));
  fs.writeFileSync(path.join(root,'code.txt'),'committed baseline\n');
  for(const args of [['init','-q'],['add','.'],['-c','user.name=Review Test','-c','user.email=test@example.invalid','commit','-qm','baseline']])execFileSync('git',args,{cwd:root});
  return root;
}

test('Review captures selected dirty/new files, preserves source HEAD and edits, verifies integrity and remains discoverable',async t=>{
  const root=fixture(t),head=execFileSync('git',['rev-parse','HEAD'],{cwd:root,encoding:'utf8'});
  fs.writeFileSync(path.join(root,'code.txt'),'current dirty edit\n');
  fs.mkdirSync(path.join(root,'module'));fs.writeFileSync(path.join(root,'module/new.txt'),'new source file\n');
  fs.writeFileSync(path.join(root,'.git/hooks/pre-commit'),'#!/bin/sh\nexit 99\n',{mode:0o755});
  const captured=await captureReview({repo_root:root,sources:['code.txt','module/new.txt']},runProcess);
  const snapshot=captured.origin.repo_root;
  assert.equal(fs.readFileSync(path.join(snapshot,'code.txt'),'utf8'),'current dirty edit\n');
  assert.equal(fs.readFileSync(path.join(snapshot,'module/new.txt'),'utf8'),'new source file\n');
  assert.equal(execFileSync('git',['rev-parse','HEAD'],{cwd:root,encoding:'utf8'}),head);
  assert.equal(fs.readFileSync(path.join(root,'code.txt'),'utf8'),'current dirty edit\n');
  assert.deepEqual(reviewOrigin(snapshot),captured.origin);
  const verified=await runProcess(captured.verify_argv[0],captured.verify_argv.slice(1),snapshot,5000);
  assert.equal(verified.code,0,verified.stderr);assert.match(verified.stdout,/selected_files_unchanged/);
  recordReview(captured,{runId:'test-run'});
  const catalog=reviewCatalog(root);assert.equal(catalog.review_runs.length,1);
  assert.equal(catalog.review_runs[0].next_call.arguments.repo_root,snapshot);
  assert.equal(catalog.review_runs[0].next_call.arguments.run_id,'test-run');
  fs.writeFileSync(path.join(snapshot,'code.txt'),'snapshot modified\n');
  const changed=await runProcess(captured.verify_argv[0],captured.verify_argv.slice(1),snapshot,5000);
  assert.notEqual(changed.code,0);assert.match(changed.stderr,/Captured file changed/);
  assert.equal(fs.readFileSync(path.join(root,'code.txt'),'utf8'),'current dirty edit\n');
});

test('Review rejects traversal, duplicate paths, symlinks, metadata, oversized input and redirected catalogs before snapshot creation',async t=>{
  const root=fixture(t);
  fs.symlinkSync('code.txt',path.join(root,'link.txt'));
  for(const sources of [['../code.txt'],['/etc/hosts'],['.git/config'],['.leio-code/x'],['code.txt','code.txt'],['link.txt']])await assert.rejects(captureReview({repo_root:root,sources},runProcess));
  fs.writeFileSync(path.join(root,'large.bin'),Buffer.alloc(4*1024*1024+1));
  await assert.rejects(captureReview({repo_root:root,sources:['large.bin']},runProcess),/byte limit/);
  assert.equal(fs.existsSync(path.join(root,'.leio-code')),false);
  const elsewhere=fs.mkdtempSync(path.join(os.tmpdir(),'leio-review-redirect-'));t.after(()=>fs.rmSync(elsewhere,{recursive:true,force:true}));
  fs.symlinkSync(elsewhere,path.join(root,'.leio-code'));
  await assert.rejects(captureReview({repo_root:root,sources:['code.txt']},runProcess),/symlinks/);
  assert.deepEqual(fs.readdirSync(elsewhere),[]);
});

test('Read-only preset disables external integrations and public review forbids caller execution overrides',async t=>{
  const root=fixture(t),binary=path.join(root,'mock-codex');fs.writeFileSync(binary,'#!/bin/sh\nexit 0\n',{mode:0o755});
  const preset=codexArguments({repo_root:root,task:'Review only',codex_binary:binary},{readOnly:true,servers:['source-tools']});
  assert.ok(!preset.argv.includes('--ignore-user-config'));assert.ok(preset.argv.includes('--ignore-rules'));
  assert.ok(preset.argv.includes('mcp_servers.source-tools.enabled=false'));
  assert.throws(()=>codexArguments({repo_root:root,task:'Review only',codex_binary:binary},{readOnly:true,servers:['name.with.dots']}),/review was not started/);
  assert.equal(preset.argv[preset.argv.indexOf('--sandbox')+1],'read-only');
  assert.ok(preset.argv.includes('approval_policy="never"'));assert.ok(preset.argv.includes('plugins'));assert.ok(preset.argv.includes('hooks'));
  const handler=createTaskHandler({runProcess});
  await assert.rejects(handler({action:'review',repo_root:root,sources:['code.txt'],argv:['true']}),/argv is only valid for submit/);
  await assert.rejects(handler({action:'review',repo_root:root,sources:['code.txt'],verify_argv:['true']}),/verify_argv is only valid for submit/);
  await assert.rejects(handler({action:'submit',repo_root:root,sources:['code.txt']}),/sources is only valid for review/);
});

test('Maximum Unicode path selection remains readable through provenance and review catalog',async t=>{
  const root=fixture(t),sources=Array.from({length:64},(_,i)=>[String(i).padStart(2,'0')+'界'.repeat(58),...Array(3).fill('界'.repeat(60))].join('/'));
  for(const relative of sources){fs.mkdirSync(path.dirname(path.join(root,relative)),{recursive:true});fs.writeFileSync(path.join(root,relative),'x');}
  const captured=await captureReview({repo_root:root,sources},runProcess);
  assert.ok(fs.statSync(path.join(captured.origin.repo_root,'.leio-code/review-source.json')).size>32768);
  assert.equal(reviewOrigin(captured.origin.repo_root).files.length,64);
  recordReview(captured,{runId:'unicode-test'});
  assert.equal(reviewCatalog(root).review_runs[0].source_files,64);
});

test('Review captures literal filenames that would otherwise invoke Git pathspec magic',async t=>{
  const root=fixture(t),relative=':(invalid)file';fs.writeFileSync(path.join(root,relative),'literal file\n');
  const captured=await captureReview({repo_root:root,sources:[relative]},runProcess);
  assert.equal(fs.readFileSync(path.join(captured.origin.repo_root,relative),'utf8'),'literal file\n');
  const verified=await runProcess(captured.verify_argv[0],captured.verify_argv.slice(1),captured.origin.repo_root,5000);
  assert.equal(verified.code,0,verified.stderr);
});

test('A corrupt or missing catalog receipt does not hide healthy reviews and recovers confined run references',async t=>{
  const root=fixture(t);
  const healthy=await captureReview({repo_root:root,sources:['code.txt']},runProcess);
  recordReview(healthy,{runId:'healthy-run'});
  const damaged=await captureReview({repo_root:root,sources:['code.txt']},runProcess);
  const runs=path.join(damaged.origin.repo_root,'.leio-code/harness/runs');
  fs.mkdirSync(path.join(runs,'recoverable-run'),{recursive:true});
  fs.writeFileSync(path.join(damaged.owner,'record.json'),'{partial');
  const catalog=reviewCatalog(root);
  assert.equal(catalog.review_runs.length,2);
  assert.ok(catalog.review_runs.some(run=>run.run_id==='healthy-run' && !run.recovered));
  const recovered=catalog.review_runs.find(run=>run.run_id==='recoverable-run');
  assert.equal(recovered.recovered,true);assert.equal(recovered.next_call.arguments.repo_root,damaged.origin.repo_root);
  assert.equal(catalog.review_catalog_incomplete,true);assert.ok(catalog.review_warnings.length);
  fs.unlinkSync(path.join(damaged.owner,'record.json'));
  assert.ok(reviewCatalog(root).review_runs.some(run=>run.run_id==='recoverable-run'));
  const external=fs.mkdtempSync(path.join(os.tmpdir(),'leio-review-outside-'));t.after(()=>fs.rmSync(external,{recursive:true,force:true}));
  fs.mkdirSync(path.join(external,'outside-run'));fs.rmSync(runs,{recursive:true});fs.symlinkSync(external,runs);
  const redirected=reviewCatalog(root);
  assert.equal(redirected.review_runs.length,1);assert.equal(redirected.review_runs[0].run_id,'healthy-run');
  assert.equal(redirected.review_catalog_incomplete,true);
});

test('Corrupt or missing source provenance falls back to a validated catalog receipt without repairing disk',async t=>{
  const root=fixture(t),capture=await captureReview({repo_root:root,sources:['code.txt']},runProcess);
  recordReview(capture,{runId:'test-run'});
  const receipt=path.join(capture.origin.repo_root,'.leio-code/review-source.json');
  fs.writeFileSync(receipt,JSON.stringify({...capture.origin,files:[{path:42,bytes:'wrong',sha256:null}]}));
  assert.throws(()=>reviewOrigin(capture.origin.repo_root),/Invalid review source receipt/);
  const recovered=inspectReviewOrigin(capture.origin.repo_root);
  assert.deepEqual(recovered.origin,capture.origin);assert.equal(recovered.review_warnings[0].kind,'review_source_recovered');
  assert.equal(JSON.parse(fs.readFileSync(receipt,'utf8')).files[0].path,42);
  fs.unlinkSync(receipt);
  assert.deepEqual(inspectReviewOrigin(capture.origin.repo_root).origin,capture.origin);
  fs.unlinkSync(path.join(capture.owner,'record.json'));
  const unavailable=inspectReviewOrigin(capture.origin.repo_root);
  assert.equal(unavailable.origin,null);assert.equal(unavailable.review_warnings[0].kind,'review_source_unavailable');
});
