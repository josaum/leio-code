import assert from "node:assert/strict";
import test from "node:test";
import { contextNextCalls, graphNextCalls, navNextCalls, formatBaseline, summarizeBaseline } from "./workflow.js";

test("baseline health preserves scope, warnings and nonzero execution failures", () => {
  const envelope = { summary: "baseline", entities: [{ doctor: "self-contract", warning_count: 2 }], warnings: ["one", "two"] };
  const warning = summarizeBaseline({ code: 0 }, envelope);
  assert.equal(warning.status, "warnings");
  assert.equal(warning.warning_count, 2);
  assert.equal(warning.doctor_count, 1);
  assert.deepEqual(warning.doctors, [{ name: "self-contract", warning_count: 2 }]);
  assert.match(formatBaseline(warning), /Baseline doctors: warnings/);
  const failed = summarizeBaseline({ code: 3, stderr: "repair the installed binary" }, { ...envelope, entities: [], warnings: [] });
  assert.equal(failed.status, "failed");
  assert.equal(failed.exit_code, 3);
  assert.match(failed.error, /repair the installed binary/);
  assert.equal(summarizeBaseline({ code: 0 }, null).status, "unavailable");
  assert.equal(summarizeBaseline({ code: 0 }, {}).status, "unavailable");
});

const navigationScope = { repoRoot: "/selected-tree", indexPath: "/selected-index.json", session: "agent-a" };
const graphSymbol = { symbol: "urn:leio:code:symbol:repo:handler:1", qual_name: "handlers::run", name: "run", path: "src/handlers.rs", kind: "function" };

test("graph follow-ups preserve exact symbol identity and the invoking scope", () => {
  const calls = graphNextCalls({ entities: [{ resolved_symbol: graphSymbol }] }, { ...navigationScope, kind: "callers-of" });
  assert.equal(calls.length, 4);
  assert.equal(calls[0].arguments.needle, graphSymbol.symbol);
  assert.equal(calls[0].arguments.kind, "callsites-of");
  const nav = calls.find((call) => call.tool === "leio_code_nav");
  assert.deepEqual(nav.arguments, { repo_root: "/selected-tree", index_path: "/selected-index.json", session: "agent-a", kind: "goto", needle: graphSymbol.symbol });
  for (const call of calls) {
    assert.equal(call.arguments.repo_root, navigationScope.repoRoot);
    assert.equal(call.arguments.index_path, navigationScope.indexPath);
    if (call.tool === "leio_code_graph") assert.equal(call.arguments.session, undefined);
  }
  assert.deepEqual(graphNextCalls(null, navigationScope), []);
});

test("ambiguous graph symbols offer distinct bounded queries instead of a fuzzy jump", () => {
  const rows = Array.from({ length: 6 }, (_, i) => ({ ...graphSymbol, symbol: `urn:leio:symbol:${i}`, path: `src/${i}.rs` }));
  const calls = graphNextCalls({ entities: [rows[0], ...rows] }, { ...navigationScope, kind: "callers-of" });
  assert.equal(calls.length, 4);
  assert.equal(new Set(calls.map((call) => call.arguments.needle)).size, 4);
  assert.ok(calls.every((call) => call.tool === "leio_code_graph" && call.arguments.kind === "callers-of"));
});

test("inherited host sessions use the same safe slug as the CLI", () => {
  const envelope = { entities: [{ resolved_symbol: graphSymbol }] };
  for (const [raw, expected] of [["w0t0p0:host-session", "w0t0p0-host-session"], ["agent/one", "agent-one"], ["x".repeat(80), "x".repeat(64)], ["---", undefined]]) {
    const calls = graphNextCalls(envelope, { ...navigationScope, kind: "callers-of", session: raw });
    assert.equal(calls.find((call) => call.tool === "leio_code_nav").arguments.session, expected);
  }
});

test("file graph recommendations obey the graph catalog and do not treat types as calls", () => {
  const envelope = { entities: [{ resolved_file: { path: "src/lib.rs" } }, { ...graphSymbol, kind: "struct" }] };
  const calls = graphNextCalls(envelope, { ...navigationScope, kind: "symbols-in", graphKinds: ["resolved-importers-of"] });
  assert.ok(calls.some((call) => call.tool === "leio_code_nav"));
  assert.ok(calls.filter((call) => call.tool === "leio_code_graph").every((call) => call.arguments.kind === "resolved-importers-of"));
});

test("lattice follow-ups select returned indices and preserve the active session", () => {
  const current = { role: "current", kind: "fca_concept", symbol: "fca:child", path: "rust", concept: "fca:child" };
  const envelope = { entities: [current, { role: "result", index: 2, kind: "fca_concept", symbol: "fca:parent", path: "rust" }], meta: { action: "parent", current_concept: "fca:child", history_len: 1, session: { id: "agent-a" } } };
  const calls = navNextCalls(envelope, navigationScope);
  assert.equal(calls[0].arguments.kind, "select");
  assert.equal(calls[0].arguments.index, 2);
  assert.ok(calls.some((call) => call.arguments.kind === "back"));
  assert.ok(calls.every((call) => call.tool === "leio_code_nav" && call.arguments.session === "agent-a"));
  assert.ok(calls.length <= 4);
  assert.ok(calls.every((call) => !["callers", "callees"].includes(call.arguments.kind)));
});

test("nav suggestions avoid stale result selection after history moves", () => {
  const envelope = { entities: [{ role: "current", path: "src/lib.rs", symbol: "run", kind: "function" }, { role: "result", index: 0, path: "src/old.rs", symbol: "old", kind: "function" }], meta: { action: "back", history_len: 0, future_len: 1, session: { id: "agent-a" } } };
  const calls = navNextCalls(envelope, navigationScope);
  assert.ok(calls.some((call) => call.arguments.kind === "callers"));
  assert.ok(calls.some((call) => call.arguments.kind === "forward"));
  assert.ok(calls.every((call) => call.arguments.kind !== "select" && call.arguments.kind !== "parent"));
  assert.deepEqual(navNextCalls(null, navigationScope), []);
});

test("navigation selection distinguishes canonical headings with identical labels", () => {
  const node = { kind: "wiki_section", path: "docs/runbook.md", symbol: "Setup" };
  const envelope = { entities: [
    { ...node, role: "current", section: "section:docs/runbook.md#2" },
    { ...node, role: "result", section: "section:docs/runbook.md#12", index: 0 },
    { ...node, role: "result", index: 1 },
  ], meta: { action: "related" } };
  const selectors = navNextCalls(envelope, navigationScope).filter((call) => call.arguments.kind === "select");
  assert.deepEqual(selectors.map((call) => call.arguments.index), [0]);
});

test("active lattice traversal takes precedence over history suggestions", () => {
  const envelope = { entities: [
    { role: "current", kind: "fca_concept", symbol: "fca:root", path: "rust", concept: "fca:root" },
    ...[0, 1].map((index) => ({ role: "result", kind: "fca_concept", symbol: `fca:child:${index}`, path: "rust", index })),
  ], meta: { action: "child", history_len: 3, future_len: 2 } };
  const calls = navNextCalls(envelope, navigationScope);
  assert.equal(calls.length, 4);
  assert.ok(calls.some((call) => call.arguments.kind === "parent"));
  assert.deepEqual(calls.slice(0, 2).map((call) => call.arguments.kind), ["select", "select"]);
});

test("continuation copies only returned query fields and pins the original scope", () => {
  const envelope = { entities: [{ role: "current", path: "src/lib.rs", symbol: "run", kind: "function" }], meta: {
    action: "here", result_page: { offset: 0, limit: 5, returned: 5, has_more: true, next_offset: 5,
      query: { kind: "callers", repo_root: "/wrong-tree", session: "other-agent", arguments: { kind: "reset" } } },
  } };
  const calls = navNextCalls(envelope, navigationScope);
  const next = calls.find((call) => call.arguments.offset === 5);
  assert.ok(next);
  assert.deepEqual(next.arguments, { repo_root: "/selected-tree", index_path: "/selected-index.json", session: "agent-a", kind: "callers", offset: 5, limit: 5 });
  envelope.meta.result_page.query.kind = "reset";
  assert.ok(navNextCalls(envelope, navigationScope).every((call) => call.arguments.offset === undefined));
});

test("lattice rebuild suggestions are explicit and do not interrupt code traversal", () => {
  const lattice = { state: "unverified", rebuild_required: true };
  const concept = { role: "current", kind: "fca_concept", symbol: "fca:root", concept: "fca:root" };
  const calls = navNextCalls({ entities: [concept], meta: { action: "parent", lattice } }, navigationScope);
  assert.deepEqual(calls[0].arguments, { repo_root: "/selected-tree", index_path: "/selected-index.json", kind: "formal-context" });
  assert.equal(calls[0].tool, "leio_code_export");
  const codeCalls = navNextCalls({ entities: [{ role: "current", kind: "function", path: "src/lib.rs", symbol: "run" }], meta: { action: "callers", lattice } }, navigationScope);
  assert.ok(codeCalls.every((call) => call.tool !== "leio_code_export"));
  assert.ok(codeCalls.every((call) => call.arguments.kind !== "callers"));
  assert.ok(codeCalls.some((call) => call.arguments.kind === "callees"));
});

test("skipped suites never count as completed baseline checks", () => {
  const summary = summarizeBaseline({ code: 0 }, {
    summary: "baseline: ran 1 doctor",
    entities: [
      { doctor: "deploy", skipped: true },
      { doctor: "self-contract", warning_count: 0 },
    ],
    warnings: [],
    meta: { doctor_count: 1 },
  });
  assert.equal(summary.doctor_count, 1);
  assert.equal(summary.skipped_count, 1);
  assert.deepEqual(summary.doctors, [{ name: "self-contract", warning_count: 0 }]);
  assert.match(formatBaseline(summary), /1 checked, 1 skipped, 0 warnings/);
});

test("context follow-ups are bounded, deduplicated and pinned to the invoking tree", () => {
  const query = { tool: "leio_code_graph", kind: "callers-of", needle: "handler", reason: "inspect callers", repo_root: "/wrong-tree", arguments: { repo_root: "/wrong-tree" } };
  const queries = [query, query,
    { ...query, tool: "leio_code_watch" },
    { ...query, kind: "unsupported" },
    { ...query, needle: "" },
    ...Array.from({ length: 8 }, (_, i) => ({ ...query, needle: `handler_${i}` })),
  ];
  const envelope = { entities: [{ graph_queries: queries }] };
  const calls = contextNextCalls(envelope, { repoRoot: "/selected-tree", indexPath: "/selected-index.json", graphKinds: ["callers-of"] });
  assert.equal(calls.length, 4);
  assert.deepEqual(calls[0], {
    tool: "leio_code_graph",
    arguments: { repo_root: "/selected-tree", index_path: "/selected-index.json", kind: "callers-of", needle: "handler" },
    reason: "inspect callers",
  });
  assert.equal(new Set(calls.map((call) => call.arguments.needle)).size, 4);
  for (const call of calls) assert.equal(call.arguments.repo_root, "/selected-tree");
  assert.deepEqual(contextNextCalls(null, { repoRoot: "/selected-tree" }), []);
});
