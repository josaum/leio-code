// Verify the installed transport, context identity and durable cursor history.
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, rm, realpath } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { Client } from '../mcp/node_modules/@modelcontextprotocol/sdk/dist/esm/client/index.js';
import { StdioClientTransport } from '../mcp/node_modules/@modelcontextprotocol/sdk/dist/esm/client/stdio.js';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const fixture = await realpath(await mkdtemp(path.join(os.tmpdir(), 'leio-stdio-install-')));
const transport = new StdioClientTransport({
  command: process.execPath,
  args: [path.join(root, 'mcp/index.js')],
  env: { ...process.env, LEIO_DISABLE_EVENTS: '1', CLAUDE_PLUGIN_ROOT: root },
  stderr: 'pipe',
});
const client = new Client({ name: 'leio-install-check', version: '1.0.0' });
transport.stderr?.resume();
try {
  await mkdir(path.join(fixture, 'src'));
  await writeFile(path.join(fixture, 'Cargo.toml'), '[package]\nname="install-check"\nversion="0.1.0"\nedition="2021"\n');
  await writeFile(path.join(fixture, 'src/lib.rs'), 'pub fn caller() { callee(); }\npub fn callee() {}\n');
  await client.connect(transport);
  const { tools } = await client.listTools();
  assert.equal(tools.length, 18, 'Expected the local 18-tool distribution');
  assert.ok(tools.some(tool => tool.name === 'leio_code_nav'));
  const call = async (name, args) => {
    const result = await client.callTool({ name, arguments: { repo_root: fixture, ...args } });
    assert.ok(!result.isError, `${name} failed: ${JSON.stringify(result.content)}`);
    return result.structuredContent;
  };
  const context = await call('leio_code_context', { task: 'caller callee', limit: 2 });
  assert.equal(context.orientation.provider.transport, 'stdio');
  assert.equal(context.orientation.index.repository, fixture);
  const graph = await call('leio_code_graph', { kind: 'symbols-in', needle: 'src/lib.rs' });
  const caller = graph.envelope.entities.find(row => row.name === 'caller');
  assert.ok(caller?.symbol, 'Graph did not return a stable caller identity');
  const nav = args => call('leio_code_nav', { session: 'install-check', ...args });
  const cursor = result => result.envelope.entities.find(row => row.role === 'current');
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
} finally {
  await client.close();
  await rm(fixture, { recursive: true, force: true });
}
