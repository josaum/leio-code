#!/usr/bin/env python3
"""Requires PyLD; exercise CLI persistence and RDF conversion in a temporary repo."""
import json,pathlib,subprocess,tempfile
from pyld import jsonld
root=pathlib.Path(tempfile.mkdtemp(prefix='leio-workflow-smoke-'))
plan={'request':'Build a verified local artifact','constraints':[],'questions':[],'acceptance':['artifact contains built'],'steps':[{'name':'build','kind':'build','argv':['/bin/sh','-c','printf built > artifact.txt'],'timeout_ms':5000,'retry_safe':True,'artifacts':['artifact.txt']}]}
(root/'plan.json').write_text(json.dumps(plan))
(root/'evidence.json').write_text(json.dumps({'repo':str(root),'limitation':'synthetic smoke fixture, no repository architecture claim'}))
import argparse
parser=argparse.ArgumentParser()
parser.add_argument('--binary', default=str(pathlib.Path(__file__).resolve().parents[1]/'target/debug/leio-harness'))
binary=parser.parse_args().binary
def call(action,*args):
 return json.loads(subprocess.check_output([binary,'workflow','--dir',str(root/'run'),'--action',action,*args]))
s=call('init','--input',str(root/'plan.json'),'--repo',str(root))
call('evidence','--input',str(root/'evidence.json'))
call('confirm','--approval',s['digest']);call('approve','--approval',s['digest'])
s=call('execute','--approval',s['digest'])
assert s['state']=='completed'
doc=json.loads((root/'run/state.jsonld').read_text())
expanded=jsonld.expand(doc)
nquads=jsonld.to_rdf(doc,{'format':'application/n-quads'})
assert doc['@id'] in nquads
assert 'http://www.w3.org/1999/02/22-rdf-syntax-ns#JSON' in nquads
assert (root/'artifact.txt').read_text()=='built'
(root/'run/state.nq').write_text(nquads)
print(json.dumps({'state':s['state'],'jsonld':str(root/'run/state.jsonld'),'rdf_quads':len(nquads.splitlines()),'expanded_nodes':len(expanded)}))
