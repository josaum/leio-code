// Verify the installed transport, context identity and durable cursor history.
import assert from 'node:assert/strict';
import {STDIO_TOOL_CATALOG} from '../mcp/mcp-spec-2025-11-25.js';
import { mkdtemp, mkdir, writeFile, rm, realpath } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { Client } from '../mcp/node_modules/@modelcontextprotocol/sdk/dist/esm/client/index.js';
import { StdioClientTransport } from '../mcp/node_modules/@modelcontextprotocol/sdk/dist/esm/client/stdio.js';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const entry = process.argv[2] ? path.resolve(process.argv[2]) : path.join(root, 'mcp/index.js');
const fixture = await realpath(await mkdtemp(path.join(os.tmpdir(), 'LEIO stdio café-')));
const transport = new StdioClientTransport({
  command: process.execPath,
  args: [entry],
  env: { ...process.env, LEIO_DISABLE_EVENTS: '1' },
  stderr: 'pipe',
});
const client = new Client({ name: 'leio-install-check', version: '1.0.0' });
let serverStderr = '';
transport.stderr?.setEncoding('utf8').on('data', (chunk) => {
  serverStderr = (serverStderr + chunk).slice(-8192);
});
try {
  await mkdir(path.join(fixture, 'src'));
  await writeFile(path.join(fixture, 'Cargo.toml'), '[package]\nname="install-check"\nversion="0.1.0"\nedition="2021"\n');
  await writeFile(path.join(fixture, 'src/lib.rs'), 'pub fn caller() { callee(); }\npub fn callee() {}\n');
  await client.connect(transport);
  const { tools } = await client.listTools();
  assert.deepEqual(tools.map(tool=>tool.name).sort(),Object.keys(STDIO_TOOL_CATALOG).sort(), 'Installed tools match the canonical stdio catalog');
  assert.ok(tools.some(tool => tool.name === 'leio_code_task'));
  assert.match(client.getInstructions(), /leio_code_task/);
  assert.ok(tools.some(tool => tool.name === 'leio_code_nav'));
  const call = async (name, args) => {
    const result = await client.callTool({ name, arguments: { repo_root: fixture, ...args } });
    assert.ok(!result.isError, `${name} failed: ${JSON.stringify(result.content)}`);
    return result.structuredContent;
  };
  const context = await call('leio_code_context', { task: 'caller callee', limit: 2 });
  assert.equal(context.orientation.provider.transport, 'stdio');
  assert.equal(context.orientation.index.repository, fixture);
  const relativeFile = 'src/lib.rs';
  const graph = await call('leio_code_graph', { kind: 'symbols-in', needle: relativeFile });
  const caller = graph.envelope.entities.find(row => row.name === 'caller');
  assert.ok(caller?.symbol, `Graph did not return a stable caller identity: ${graph.envelope.summary}`);
  const nativeFile = path.join('src', 'lib.rs');
  if (nativeFile !== relativeFile) {
    const nativeGraph = await call('leio_code_graph', { kind: 'symbols-in', needle: nativeFile });
    assert.equal(nativeGraph.envelope.entities.find(row => row.name === 'caller')?.symbol, caller.symbol);
  }
  const nav = args => call('leio_code_nav', { session: 'install-check', ...args });
  const cursor = result => result.envelope.entities.find(row => row.role === 'current');
  const file = await nav({ kind: 'goto', needle: path.join(fixture, nativeFile) });
  assert.equal(cursor(file)?.path, relativeFile);
  await nav({ kind: 'goto', needle: caller.symbol });
  const here = await nav({ kind: 'here' });
  assert.ok(JSON.stringify(here).includes('src/lib.rs'));
  await nav({ kind: 'callees' });
  const afterList = await nav({ kind: 'here' });
  assert.deepEqual(cursor(afterList), cursor(here), 'Listing moved the cursor');
  await nav({ kind: 'select', index: 0 });
  const selected = await nav({ kind: 'here' });
  assert.equal(cursor(selected).symbol, 'callee');
  await nav({ kind: 'back' });
  const restored = await nav({ kind: 'here' });
  assert.deepEqual(cursor(restored), cursor(here), 'Back did not restore the cursor');
  console.error(`Verified ${tools.length} stdio tools, local CLI identity, task context and session cursor.`);
} catch (error) {
  if (serverStderr) console.error(serverStderr);
  throw error;
} finally {
  await client.close();
  await rm(fixture, { recursive: true, force: true });
}
