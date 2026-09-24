import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, writeFileSync, realpathSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

import { STDIO_TOOL_CATALOG } from "./mcp-spec-2025-11-25.js";
import { verifyEvidenceContract } from "./evidence-contract.js";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..");

function resolveLeioBin() {
  const candidates = [
    process.env.LEIO_CODE_BIN,
    join(homedir(), ".cargo", "bin", "leio-code"),
    join(repoRoot, "target", "release", "leio-code"),
    join(repoRoot, "target", "debug", "leio-code"),
  ];
  for (const candidate of candidates) {
    if (candidate && existsSync(candidate)) {
      return candidate;
    }
  }
  throw new Error("leio-code binary not found; set LEIO_CODE_BIN");
}

function write(root, rel, body) {
  const path = join(root, rel);
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, body);
}

function stageFixture() {
  const root = mkdtempSync(join(tmpdir(), "leio-mcp-"));
  write(
    root,
    "Cargo.toml",
    "[package]\nname = \"mcp-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
  );
  write(
    root,
    "src/lib.rs",
    "pub fn encode_payload() {\n    let _ = std::env::var(\"MCP_SURFACE_TOKEN\");\n}\n",
  );
  write(
    root,
    "app.py",
    "import os\nTOKEN = os.getenv(\"MCP_SURFACE_TOKEN\")\n",
  );
  write(root, "README.md", "# MCP fixture\n\n## Hours\n\nOpen 05:30-23:00.\n");
  execFileSync("git", ["init", "--quiet", root]);
  execFileSync("git", ["-C", root, "add", "Cargo.toml", "src/lib.rs", "app.py", "README.md"]);
  return root;
}

function textOf(result) {
  return (result.content ?? [])
    .filter((item) => item.type === "text")
    .map((item) => item.text)
    .join("\n");
}

function structured(result, name) {
  if (!result?.structuredContent) {
    throw new Error(
      `${name} missing structuredContent: ${JSON.stringify(result, null, 2).slice(0, 2000)}`,
    );
  }
  return result.structuredContent;
}

async function withClient(t, fn) {
  const fixture = stageFixture();
  const transport = new StdioClientTransport({
    command: process.execPath,
    args: [join(here, "index.js")],
    env: {
      ...process.env,
      LEIO_CODE_BIN: resolveLeioBin(),
      LEIO_CODE_REPO_ROOT: fixture,
      LEIO_SESSION: "mcp:tools-test",
    },
    stderr: "pipe",
  });
  const client = new Client({
    name: "leio-code-mcp-tools",
    version: "2.6.0",
  });
  await client.connect(transport);
  t.after(async () => {
    await client.close().catch(() => {});
  });

  async function call(name, args = {}, timeoutMs = 60_000) {
    return client.callTool(
      {
        name,
        arguments: { repo_root: fixture, timeout_ms: timeoutMs, ...args },
      },
      undefined,
      { timeout: timeoutMs + 5_000 },
    );
  }

  return fn({ client, call, fixture });
}

test("stdio MCP lists the full catalog", async (t) => {
  await withClient(t, async ({ client }) => {
    const listed = await client.listTools();
    const names = listed.tools.map((tool) => tool.name).sort();
    assert.deepEqual(names, Object.keys(STDIO_TOOL_CATALOG).sort());
    const guide = listed.tools.find((tool) => tool.name === "leio_code_guide");
    assert.equal(guide.outputSchema.properties.next_tools.type, "array");
    for (const name of ["leio_code_graph", "leio_code_nav"]) {
      assert.equal(listed.tools.find((tool) => tool.name === name).outputSchema.properties.next_calls.type, "array");
    }
    assert.equal(listed.tools.find((tool) => tool.name === "leio_code_nav").inputSchema.properties.session.type, "string");
    const nav = listed.tools.find((tool) => tool.name === "leio_code_nav");
    assert.equal(nav.inputSchema.properties.offset.type, "integer");
    assert.equal(nav.inputSchema.properties.limit.maximum, 100);
    const envelopeSchema = nav.outputSchema.properties.envelope.anyOf.find((schema) => schema.type === "object");
    const pageSchema = envelopeSchema.properties.meta.properties.result_page.anyOf.find((schema) => schema.type === "object");
    assert.equal(pageSchema.properties.has_more.type, "boolean");
    assert.ok(pageSchema.required.includes("next_offset"));
  });
});

test("graph-to-cursor follow-ups are executable and per-call sessions stay isolated", async (t) => {
  await withClient(t, async ({ call, fixture }) => {
    await call("leio_code_index");
    const graph = await call("leio_code_graph", { kind: "symbols-in", needle: "src/lib.rs" });
    assert.equal(graph.isError, false, textOf(graph));
    const suggested = graph.structuredContent.next_calls.find((entry) => entry.tool === "leio_code_nav");
    assert.ok(suggested, JSON.stringify(graph.structuredContent.next_calls));
    assert.ok(suggested.arguments.needle.startsWith("urn:"));
    assert.equal(suggested.arguments.session, "mcp-tools-test");
    assert.equal((await call(suggested.tool, suggested.arguments)).isError, false);
    const first = await call(suggested.tool, { ...suggested.arguments, session: "agent-a" });
    assert.equal(first.isError, false, textOf(first));
    const current = first.structuredContent.envelope.entities.find((row) => row.role === "current");
    assert.equal(current.path, "src/lib.rs");
    assert.equal(current.graph_symbol, suggested.arguments.needle);
    assert.equal(first.structuredContent.envelope.meta.session.id, "agent-a");
    assert.ok(first.structuredContent.next_calls.some((entry) => entry.arguments.kind === "callers"));
    const next = first.structuredContent.next_calls.find((entry) => entry.arguments.kind === "callers");
    assert.equal(next.arguments.session, "agent-a");
    assert.equal(next.arguments.repo_root, fixture);
    assert.equal((await call(next.tool, next.arguments)).isError, false);
    await call("leio_code_nav", { kind: "goto", needle: "TOKEN", session: "agent-b" });
    const restored = await call("leio_code_nav", { kind: "here", session: "agent-a" });
    assert.equal(restored.structuredContent.envelope.entities.find((row) => row.role === "current").graph_symbol, current.graph_symbol);
    assert.equal(restored.structuredContent.envelope.meta.session.id, "agent-a");
    const invalid = await call("leio_code_nav", { kind: "here", session: "../../shared" });
    assert.equal(invalid.isError, true);
    assert.equal((await call("leio_code_nav", { kind: "here", session: "---" })).isError, true);
  });
});

test("exact file navigation and sequential pages preserve source evidence and cursor identity", async (t) => {
  await withClient(t, async ({ call, fixture }) => {
    write(fixture, "src/lib.rs", "pub fn entry() { first(); second(); third(); }\nfn first() {}\nfn second() {}\nfn third() {}\n");
    await call("leio_code_index");
    const session = "paged-investigation";
    const file = await call("leio_code_nav", { kind: "goto", needle: "./src/lib.rs", session });
    assert.equal(file.isError, false, textOf(file));
    const current = file.structuredContent.envelope.entities.find((row) => row.role === "current");
    assert.equal(current.kind, "file");
    assert.equal(current.path, "src/lib.rs");
    assert.equal(current.source.state, "current");
    assert.ok(current.source.text.includes("pub fn entry"));
    assert.equal(file.structuredContent.envelope.meta.lattice, undefined);
    const fullFile = await call("leio_code_nav", { kind: "here", session, full: true });
    assert.equal(fullFile.structuredContent.envelope.meta.lattice.state, "missing");
    assert.ok(file.structuredContent.next_calls.some((entry) => entry.arguments.kind === "symbols-in"));
    const graph = await call("leio_code_graph", { kind: "symbols-in", needle: "src/lib.rs" });
    const entry = graph.structuredContent.envelope.entities.find((row) => row.name === "entry");
    const atEntry = await call("leio_code_nav", { kind: "goto", needle: entry.symbol, session });
    const entryCursor = atEntry.structuredContent.envelope.entities.find((row) => row.role === "current");
    assert.ok(entryCursor.line > 0);
    assert.equal(entryCursor.line, entry.line);
    write(fixture, "src/lib.rs", "\n\npub fn entry() { first(); second(); third(); }\nfn first() {}\nfn second() {}\nfn third() {}\n");
    await call("leio_code_index");
    const movedEntry = await call("leio_code_nav", { kind: "goto", needle: entry.symbol, session });
    const movedCursor = movedEntry.structuredContent.envelope.entities.find((row) => row.role === "current");
    assert.equal(movedCursor.graph_symbol, entry.symbol);
    assert.equal(movedCursor.line, entry.line + 2);
    assert.equal(movedEntry.structuredContent.envelope.meta.history_len, atEntry.structuredContent.envelope.meta.history_len);
    const first = await call("leio_code_nav", { kind: "callees", limit: 1, session });
    assert.equal(first.isError, false, textOf(first));
    const page = first.structuredContent.envelope.meta.result_page;
    assert.equal(page.offset, 0);
    assert.equal(page.returned, 1);
    assert.equal(page.has_more, true);
    const row = first.structuredContent.envelope.entities.find((row) => row.role === "result");
    assert.equal(row.index, 0);
    assert.ok(row.line > 0);
    assert.ok(first.structuredContent.envelope.evidence.some((item) => item.line === row.line));
    const continuation = first.structuredContent.next_calls.find((entry) => entry.arguments.offset === page.next_offset);
    assert.ok(continuation);
    const second = await call(continuation.tool, continuation.arguments);
    assert.equal(second.isError, false, textOf(second));
    const nextRow = second.structuredContent.envelope.entities.find((row) => row.role === "result");
    assert.equal(nextRow.index, 0);
    assert.notEqual(nextRow.graph_symbol, row.graph_symbol);
    const selected = await call("leio_code_nav", { kind: "select", index: 0, session });
    assert.equal(selected.structuredContent.envelope.entities.find((row) => row.role === "current").graph_symbol, nextRow.graph_symbol);
    assert.ok(!selected.structuredContent.envelope.meta.result_page);
    assert.equal((await call(continuation.tool, continuation.arguments)).isError, true);
  });
});

test("documented doctor presets remain callable after lazy schema loading", async (t) => {
  await withClient(t, async ({ client, call }) => {
    const listed = await client.listTools();
    const doctor = listed.tools.find((tool) => tool.name === "leio_code_doctor");
    const graph = listed.tools.find((tool) => tool.name === "leio_code_graph");
    assert.equal(graph.inputSchema.properties.full.type, "boolean");
    const full = await call("leio_code_graph", { kind: "symbols-in", needle: "src/lib.rs", full: true });
    assert.ok(full.structuredContent.envelope_summary, "full graph diagnostics must survive lazy schema upgrade");
    for (const kind of ["baseline", "ci"]) {
      assert.equal(doctor.inputSchema.properties.kind.type, "string");
      const result = await call("leio_code_doctor", { kind });
      assert.equal(result.isError, false, textOf(result));
      assert.equal(result.structuredContent.tool_family, "doctor");
    }
  });
});

test("conversation tool reads only selected files and leaves model work pending", async (t) => {
  await withClient(t, async ({ call, fixture }) => {
    write(fixture, "chat.txt", "[01/09/26, 10:00:00] Alex: Prior context\n[02/09/26, 11:00:00] Alex: Current message\n");
    const result = await call("leio_code_conversation", { sources: ["chat.txt"], account: "Alex" });
    assert.equal(result.isError, false);
    const packet = structured(result, "conversation").envelope.entities[0];
    assert.equal(verifyEvidenceContract(result.structuredContent, result.structuredContent.evidence_contract), true);
    assert.equal(result.structuredContent.evidence_contract.transport, "stdio");
    assert.equal(packet.inventory.messages, 2);
    assert.equal(packet.authorship.probability, null);
    assert.equal(packet.category.composition_check, true);
    assert.ok(packet.method_plan.every((m) => m.status === "proposed-not-run"));
    assert.equal(existsSync(join(fixture, ".leio-code", "events", "events.ndjson")), false);
    const rejected = await call("leio_code_conversation", { sources: ["../escape.txt"] });
    assert.equal(rejected.isError, true);
    const guide = await call("leio_code_guide", { topic: "conversation" });
    assert.equal(guide.structuredContent.topic, "conversation");
    assert.match(textOf(guide), /Reference Provider/);
    assert.deepEqual(guide.structuredContent.next_tools, ["leio_code_conversation"]);
    assert.equal(existsSync(join(fixture, ".leio-code")), false);
  });
});

test("guide status capabilities index context find explain graph", async (t) => {
  await withClient(t, async ({ call }) => {
    const guide = await call("leio_code_guide", { topic: "general" });
    assert.equal(guide.isError, false);
    assert.equal(guide.structuredContent.routing_doc, "skills/leio-code/SKILL.md");
    assert.ok(guide.structuredContent.repo_root);
    const graphGuide = await call("leio_code_guide", { topic: "graph" });
    assert.equal(graphGuide.structuredContent.next_tools[0], "leio_code_graph");
    assert.equal(graphGuide.structuredContent.action_palette.recommended_start_tool, "leio_code_graph");
    assert.deepEqual(graphGuide.structuredContent.action_palette.recommended_next_tools,
      graphGuide.structuredContent.next_tools);

    const status = await call("leio_code_status");
    assert.equal(status.isError, false);
    assert.match(textOf(status), /index|workspace|generic/i);
    assert.equal(status.structuredContent.doctor_summary.scope, "baseline");
    assert.equal(status.structuredContent.doctor_summary.status, "passed");
    const baseline = await call("leio_code_doctor", { kind: "baseline" });
    assert.equal(status.structuredContent.doctor_summary.doctor_count,
      baseline.structuredContent.envelope.meta.doctor_count);
    assert.match(textOf(status), /Baseline doctors:/);
    assert.match(textOf(status), /Find kinds:/);

    const caps = await call("leio_code_capabilities");
    assert.equal(caps.isError, false);
    assert.equal(caps.structuredContent.ok, true);
    assert.equal(caps.structuredContent.tool_family, "capabilities");
    const findKinds =
      caps.structuredContent.workspace_capabilities?.find_kinds ??
      caps.structuredContent.envelope?.entities?.[0]?.find_kinds ??
      [];
    assert.ok(findKinds.includes("symbol"), JSON.stringify(findKinds));

    const indexed = await call("leio_code_index");
    assert.equal(indexed.isError, false);
    assert.equal(indexed.structuredContent.ok, true);

    const context = await call("leio_code_context", {
      task: "encode_payload MCP_SURFACE_TOKEN",
      limit: 6,
    });
    assert.equal(context.isError, false);
    assert.equal(context.structuredContent.ok, true);
    assert.equal(context.structuredContent.tool_family, "context");
    const orientation = context.structuredContent.orientation;
    assert.equal(orientation.provider.transport, "stdio");
    assert.match(orientation.provider.binary_version, /leio-code/);
    assert.equal(orientation.index.state, "available");
    assert.equal(realpathSync(orientation.index.repository), realpathSync(context.structuredContent.repo_root));
    assert.equal(orientation.index.source_freshness, "not_checked");
    assert.equal(orientation.retrieval.calibrated_confidence, false);
    assert.equal(context.structuredContent.next_calls[0].arguments.kind, "goto");
    const followup = context.structuredContent.next_calls[0];
    assert.equal(followup.tool, "leio_code_nav");
    assert.equal(followup.arguments.repo_root, context.structuredContent.repo_root);
    assert.equal((await call(followup.tool, followup.arguments)).isError, false);

    const found = await call("leio_code_find", {
      kind: "symbol",
      needle: "encode_payload",
    });
    assert.equal(found.isError, false);
    assert.equal(found.structuredContent.ok, true);
    const names = (found.structuredContent.envelope?.entities ?? []).map(
      (row) => row.name,
    );
    assert.ok(names.includes("encode_payload"), JSON.stringify(names));

    const envVar = await call("leio_code_find", {
      kind: "env-var",
      needle: "MCP_SURFACE_TOKEN",
    });
    assert.equal(envVar.structuredContent.ok, true);
    const envBlob = JSON.stringify(envVar.structuredContent.envelope);
    assert.match(envBlob, /MCP_SURFACE_TOKEN/);

    const explained = await call("leio_code_explain", {
      kind: "env-var",
      needle: "MCP_SURFACE_TOKEN",
    });
    assert.equal(explained.structuredContent.ok, true);
    assert.equal(explained.structuredContent.tool_family, "explain");

    const callers = await call("leio_code_graph", {
      kind: "callers-of",
      needle: "encode_payload",
    });
    assert.equal(callers.structuredContent.ok, true);
    assert.equal(callers.structuredContent.tool_family, "graph");

    const dead = await call("leio_code_graph", { kind: "dead-code" });
    assert.equal(dead.structuredContent.ok, true);
  });
});

test("doctor audit verify export nav knowledge watch init", async (t) => {
  await withClient(t, async ({ call, fixture }) => {
    const doctor = structured(
      await call("leio_code_doctor", { kind: "repo-hygiene" }),
      "doctor",
    );
    assert.ok(doctor.envelope || doctor.ok !== undefined, JSON.stringify(doctor));
    assert.equal(doctor.tool_family, "doctor");

    const audit = structured(
      await call("leio_code_audit", { format: "markdown", strict: false }),
      "audit",
    );
    assert.ok(audit.envelope || audit.ok !== undefined, JSON.stringify(audit));
    assert.equal(audit.tool_family, "audit");

    const verified = structured(await call("leio_code_verify"), "verify");
    assert.equal(verified.tool_family, "verify");

    const exported = structured(
      await call("leio_code_export", { kind: "formal-context" }),
      "export",
    );
    assert.equal(exported.ok, true);
    assert.equal(exported.tool_family, "export");

    const nav = structured(
      await call("leio_code_nav", { kind: "goto", needle: "Hours" }),
      "nav",
    );
    assert.equal(nav.ok, true);
    assert.equal(nav.tool_family, "nav");

    const hereNav = structured(await call("leio_code_nav", { kind: "here" }), "nav here");
    assert.equal(hereNav.ok, true);

    const compiled = structured(
      await call("leio_code_knowledge", { kind: "compile" }),
      "knowledge compile",
    );
    assert.equal(compiled.ok, true);
    assert.equal(compiled.tool_family, "knowledge");

    const wiki = structured(
      await call("leio_code_knowledge", { kind: "status" }),
      "knowledge status",
    );
    assert.ok(wiki.envelope);

    const watch = structured(
      await call("leio_code_watch", { action: "status" }),
      "watch",
    );
    assert.equal(typeof watch.running, "boolean");

    const fresh = mkdtempSync(join(tmpdir(), "leio-mcp-init-"));
    write(fresh, "hello.rs", "fn hello() {}\n");
    const inited = structured(
      await call("leio_code_init", { repo_root: fresh }),
      "init",
    );
    assert.equal(inited.ok, true);
    assert.ok(existsSync(join(fresh, ".leio-code", "config.toml")));
    assert.ok(existsSync(join(fresh, ".leio-code", "index.json")));

    assert.ok(existsSync(join(fixture, "src", "lib.rs")));
  });
});

test("MCP rejects invalid explain/graph/knowledge input as isError", async (t) => {
  await withClient(t, async ({ call }) => {
    const missingExplain = await call("leio_code_explain", { kind: "env-var" });
    assert.equal(missingExplain.isError, true);
    assert.match(textOf(missingExplain), /needle/i);

    const missingGraph = await call("leio_code_graph", { kind: "callers-of" });
    assert.equal(missingGraph.isError, true);
    assert.match(textOf(missingGraph), /needle/i);

    const missingKnowledge = await call("leio_code_knowledge", {
      kind: "explain",
    });
    assert.equal(missingKnowledge.isError, true);
    assert.match(textOf(missingKnowledge), /needle/i);

    const missingFind = await call("leio_code_find", { kind: "symbol" });
    assert.equal(missingFind.isError, true);
  });
});

test("audit json either returns an envelope or the known output-schema mismatch", async (t) => {
  await withClient(t, async ({ call }) => {
    const result = await call("leio_code_audit", { format: "json", strict: false });
    if (result.structuredContent) {
      assert.ok(result.structuredContent.tool_family === "audit");
      return;
    }
    assert.equal(result.isError, true);
    assert.match(textOf(result), /Output validation error|envelope\.summary/i);
  });
});
