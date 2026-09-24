import test from 'node:test';
import assert from 'node:assert/strict';
import {compactEditingResult} from './compact.js';
import {contextNextCalls} from './workflow.js';

test('compact editing packet keeps source once, direct opens and full diagnostics action',()=>{
 const current={role:'current',kind:'function',graph_symbol:'urn:example:run',path:'a.rs',line:1,source:{state:'current',text:'fn run() {}'},concept_details:{members:Array(100).fill({verbose:'lattice data unrelated to code traversal'})}};
 const original={content:[{type:'text',text:'repeated diagnostics'}],structuredContent:{ok:true,repo_root:'/repo',tool_family:'nav',envelope_summary:{summary:'same'},ui_hints:{},action_palette:{},envelope:{kind:'nav',summary:'at run',entities:[current,{...current,role:'result',index:0}],warnings:['lattice stale: unrelated'],meta:{navigation_mode:'graph',action:'goto',session:{id:'test'}}},next_calls:[]}};
 const compact=compactEditingResult(original,{scope:{kind:'goto',needle:'urn:example:run',session:'test'}});
 assert.ok(JSON.stringify(compact).length < JSON.stringify(original).length/3);
 assert.equal(compact.structuredContent.envelope.entities.length,1);
 assert.equal(compact.structuredContent.envelope.entities[0].source.text,'fn run() {}');
 assert.equal(compact.structuredContent.envelope.entities[0].open.arguments.needle,'urn:example:run');
 assert.equal(compact.structuredContent.envelope.meta.session.id,'test');
 assert.equal(compact.structuredContent.diagnostics.arguments.full,true);
 assert.equal(compactEditingResult(original,{full:true}),original);
 assert.equal(compactEditingResult({...original,structuredContent:{...original.structuredContent,ok:false}}).structuredContent.ok,false);
});

test('context opens a grounded definition without guessed URNs or global fuzzy names',()=>{
 const calls=contextNextCalls({entities:[{files_to_read:[{path:'src/a.rs',symbols:[{name:'run',kind:'function',line:42}]}]}]}, {repoRoot:'/repo',session:'agent-a'});
 assert.deepEqual(calls[0].arguments,{repo_root:'/repo',session:'agent-a',kind:'goto',needle:'definition:src/a.rs#42'});
});
