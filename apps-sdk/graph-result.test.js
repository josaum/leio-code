import test from "node:test";
import assert from "node:assert/strict";

import { presentHostedGraphResult } from "./graph-result.js";

function graphResult(count) {
  return {
    content: [{ type: "text", text: "full text" }],
    structuredContent: {
      ok: true,
      repo_root: "/repo",
      evidence_contract: { producer: "test" },
      envelope: {
        schema_version: "1.0",
        query_id: "graph-1",
        kind: "graph",
        summary: "graph summary",
        timing_ms: 1,
        entities: Array.from({ length: count }, (_, index) => ({
          symbol: `sym-${index}`,
        })),
        evidence: [],
        warnings: [],
        meta: { kind: "symbols-in" },
      },
    },
  };
}

test("hosted graph defaults to the compact entity window", () => {
  const presented = presentHostedGraphResult(graphResult(25), {
    scope: { kind: "symbols-in", needle: "src/lib.rs" },
  });
  assert.equal(presented.structuredContent.evidence_contract.producer, "test");
  assert.equal(presented.structuredContent.envelope.entities.length, 20);
  assert.equal(presented.structuredContent.envelope.meta.truncated, true);
  assert.equal(presented.structuredContent.envelope.meta.total, 25);
  assert.equal(presented.content[0].text, "graph summary");
});

test("hosted graph full keeps the complete result", () => {
  const original = graphResult(25);
  const presented = presentHostedGraphResult(original, { full: true });
  assert.equal(presented, original);
  assert.equal(presented.structuredContent.envelope.entities.length, 25);
});

test("hosted graph leaves a failed result unchanged", () => {
  const failed = {
    content: [{ type: "text", text: "failed" }],
    structuredContent: { ok: false, envelope: { entities: [{ symbol: "x" }] } },
  };
  assert.equal(presentHostedGraphResult(failed), failed);
});
