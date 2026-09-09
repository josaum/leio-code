import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { indexDiagnostics, retrievalAssessment } from './orientation.js';
import { contextNextCalls } from './workflow.js';

test('coverage distinguishes missing, corrupt, mismatched indexes and unchecked freshness', () => {
 const root=fs.mkdtempSync(path.join(os.tmpdir(),'leio-orientation-'));
 const artifact=path.join(root,'index.json');
 try {
  assert.equal(indexDiagnostics(root,artifact).state,'missing');
  fs.writeFileSync(artifact,'{');
  assert.equal(indexDiagnostics(root,artifact).state,'unreadable');
  fs.writeFileSync(artifact,JSON.stringify({root,version:18,indexed_at:'2026-01-01T00:00:00Z',files:[{language:'c_sharp'},{language:'razor'}]}));
  const result=indexDiagnostics(root,artifact,Date.parse('2026-01-01T00:01:00Z'));
  assert.equal(result.age_seconds,60);
  assert.equal(result.source_freshness,'not_checked');
  assert.equal(result.excluded_files,null);
  assert.deepEqual(result.indexed_languages,{c_sharp:1,razor:1});
  assert.equal(indexDiagnostics(path.join(root,'other'),artifact).state,'repository_mismatch');
 } finally {fs.rmSync(root,{recursive:true,force:true});}
});

test('context suggestions select exact ranked files instead of unrelated fuzzy symbols', () => {
 const envelope={entities:[{files_to_read:[{path:'src/Program.cs'},{path:'src/Program.cs'}],graph_queries:[{tool:'leio_code_graph',kind:'callers-of',needle:'UnrelatedMigration'}]}]};
 const calls=contextNextCalls(envelope,{repoRoot:'/repo',indexPath:'/index'});
 assert.equal(calls.length,1);
 assert.deepEqual(calls[0].arguments,{repo_root:'/repo',index_path:'/index',kind:'symbols-in',needle:'src/Program.cs'});
 assert.equal(retrievalAssessment(envelope).calibrated_confidence,false);
 assert.equal(retrievalAssessment({}).state,'no_matches');
 assert.deepEqual(contextNextCalls({entities:[{files_to_read:[],graph_queries:envelope.entities[0].graph_queries}]},{repoRoot:'/repo'}),[]);
});
