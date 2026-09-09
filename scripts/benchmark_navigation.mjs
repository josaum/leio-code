// Guided multi-step investigations through the real local stdio MCP.
// No model calls, fixes, or automatic reasoning are measured.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile, writeFile, realpath } from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import { execFileSync } from 'node:child_process';
import { performance } from 'node:perf_hooks';
import { fileURLToPath } from 'node:url';
import { Client } from '../mcp/node_modules/@modelcontextprotocol/sdk/dist/esm/client/index.js';
import { StdioClientTransport } from '../mcp/node_modules/@modelcontextprotocol/sdk/dist/esm/client/stdio.js';

const args = process.argv.slice(2);
const option = (name, fallback) => args.includes(name) ? args[args.indexOf(name) + 1] : fallback;
const flag = name => args.includes(name);
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const repo = await realpath(option('--repo', root));
const output = option('--output', path.join(os.tmpdir(), 'leio-navigation-benchmark.json'));
const repeats = Number(option('--repeats', '5'));
assert.ok(Number.isInteger(repeats) && repeats > 0 && repeats <= 100);
assert.ok(process.env.LEIO_CODE_BIN && path.isAbsolute(process.env.LEIO_CODE_BIN), 'Set LEIO_CODE_BIN to an absolute release binary');
const binary = await realpath(process.env.LEIO_CODE_BIN);
const git = (cwd, ...gitArgs) => execFileSync('git', ['-C', cwd, ...gitArgs], { encoding: 'utf8' }).trim();
const sha256 = async file => createHash('sha256').update(await readFile(file)).digest('hex');
const sourceIdentity = cwd => {
  const dirtyPaths = git(cwd, 'status', '--porcelain=v1', '--untracked-files=all').split('\n').filter(Boolean);
  return { revision: git(cwd, 'rev-parse', 'HEAD'), clean: dirtyPaths.length === 0, dirty_path_count: dirtyPaths.length };
};
const harness = sourceIdentity(root);
const inspectedSource = sourceIdentity(repo);
if (!flag('--allow-dirty')) {
  assert.ok(harness.clean, `Benchmark harness is dirty (${harness.dirty_path_count} paths); commit it or pass --allow-dirty`);
  assert.ok(inspectedSource.clean, `Inspected source is dirty (${inspectedSource.dirty_path_count} paths); commit it or pass --allow-dirty`);
}
const binaryVersion = execFileSync(binary, ['--version'], { encoding: 'utf8' }).trim();
assert.ok(binaryVersion.includes(inspectedSource.revision.slice(0, 12)), `Binary/source mismatch: source ${inspectedSource.revision.slice(0, 12)}, binary ${binaryVersion}`);
const scenarios = [
  { name: 'Investigate MCP binary selection', task: 'MCP binary resolution installed cargo executable', file: 'mcp/resolve-binary.js', caller: 'resolveBinaryPath', callee: 'findInstalledBinary' },
  { name: 'Trace context ranking', task: 'context file ranking scoring exact identifier relevance', file: 'src/context.rs', caller: 'build_context_bundle', callee: 'rank_files' },
  { name: 'Follow workflow policy dispatch', task: 'persist workflow approval retry revision inputs', file: 'crates/leio-harness/src/workflow.rs', caller: 'apply', callee: 'apply_with_deployment_policy' },
];
let client, transport;
async function connect() {
  transport = new StdioClientTransport({ command: process.execPath, args: [path.join(root, 'mcp/index.js')], env: { ...process.env, CLAUDE_PLUGIN_ROOT: root }, stderr: 'pipe' });
  client = new Client({ name: 'leio-navigation-benchmark', version: '1.0.0' });
  transport.stderr?.resume();
  await client.connect(transport);
}
async function call(name, arguments_) {
  const r = await client.callTool({ name, arguments: { repo_root: repo, ...arguments_ } });
  assert.ok(!r.isError, `${name}: ${JSON.stringify(r.content)}`);
  return r.structuredContent;
}
const cursor = r => r.envelope.entities.find(e => e.role === 'current');
const rows = [];
try {
  await connect();
  await call('leio_code_index', {});
  const orientation = await call('leio_code_context', { task: scenarios[0].task, limit: 5 });
  for (const scenario of scenarios) await call('leio_code_graph', { kind: 'symbols-in', needle: scenario.file });
  for (const scenario of scenarios) {
    const runs = [];
    for (let i = 0; i < repeats; i++) {
      const session = `benchmark-${process.pid}-${rows.length}-${i}`;
      const nav = args => call('leio_code_nav', { session, ...args });
      const started = performance.now();
      const context = await call('leio_code_context', { task: scenario.task, limit: 5 });
      const ranked = context.envelope.entities[0].files_to_read.map(r => r.path);
      assert.ok(ranked.includes(scenario.file), 'Labeled implementation missing from bounded context');
      const graph = await call('leio_code_graph', { kind: 'symbols-in', needle: scenario.file });
      const caller = graph.envelope.entities.find(e => e.name === scenario.caller);
      assert.ok(caller?.symbol);
      await nav({ kind: 'goto', needle: caller.symbol });
      const before = cursor(await nav({ kind: 'here' }));
      const listed = await nav({ kind: 'callees' });
      assert.deepEqual(cursor(await nav({ kind: 'here' })), before, 'Listing changed cursor');
      const candidates = listed.envelope.entities.filter(e => e.role === 'result');
      const selectedIndex = candidates.findIndex(e => JSON.stringify(e).includes(scenario.callee) && JSON.stringify(e).includes(scenario.file));
      assert.ok(selectedIndex >= 0, `Expected callee missing: ${JSON.stringify(candidates)}`);
      await nav({ kind: 'select', index: selectedIndex });
      const after = cursor(await nav({ kind: 'here' }));
      assert.equal(after.symbol, scenario.callee);
      await nav({ kind: 'back' });
      assert.deepEqual(cursor(await nav({ kind: 'here' })), before);
      await client.close();
      await connect();
      assert.deepEqual(cursor(await nav({ kind: 'here' })), before, 'Provider restart lost cursor');
      runs.push({ elapsed_ms: Math.round((performance.now() - started) * 10) / 10, tool_calls: 11, expected_file_rank: ranked.indexOf(scenario.file) + 1, returned_files: ranked, candidate_count: candidates.length, selected_result_index: selectedIndex, selected_symbol: after.symbol, cursor_restored: true, cursor_survived_provider_restart: true });
    }
    const sorted = runs.map(r => r.elapsed_ms).sort((a,b) => a-b);
    const median = sorted.length % 2 ? sorted[Math.floor(sorted.length/2)] : (sorted[sorted.length/2-1]+sorted[sorted.length/2])/2;
    rows.push({ ...scenario, median_ms: Math.round(median * 10) / 10, runs });
    console.error(`${scenario.name}: ${median} ms median; ${runs.length}/${repeats} completed`);
  }
  const report = {
    schema_version: 2,
    measured_at: new Date().toISOString(),
    repository: 'https://github.com/josaum/leio-code',
    harness: {
      ...harness,
      entrypoint: 'scripts/benchmark_navigation.mjs',
      entrypoint_sha256: await sha256(fileURLToPath(import.meta.url)),
      mcp_wrapper: 'mcp/index.js',
      mcp_wrapper_sha256: await sha256(path.join(root, 'mcp/index.js')),
    },
    inspected_source: inspectedSource,
    binary: { version: binaryVersion, sha256: await sha256(binary) },
    environment: { platform: os.platform(), arch: os.arch(), cpu: os.cpus()[0].model, node: process.version },
    indexed_files: orientation.orientation.index.indexed_files,
    setup: 'Fresh index then untimed context and graph warm-up. Setup excluded. Default provenance events enabled.',
    measurement: '11 sequential MCP tool calls plus one provider close/reconnect per run. Assertions included in elapsed time. Fixed labeled file and edge selected by script; no LLM reasoning time.',
    limitations: 'Three guided development scenarios, not held-out tasks or bug-solving results. No comparator, token-saving, productivity or graph-completeness claim. Unselected graph candidates are not validated.',
    rows,
  };
  assert.deepEqual(sourceIdentity(root), harness, 'Benchmark harness changed during the run');
  assert.deepEqual(sourceIdentity(repo), inspectedSource, 'Inspected source changed during the run');
  await writeFile(output, JSON.stringify(report,null,2)+'\n');
} finally { await client?.close(); }
