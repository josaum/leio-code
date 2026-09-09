import test from "node:test";
import assert from "node:assert/strict";

import {
  LEIO_EVIDENCE_CONTRACT,
  buildEvidenceContract,
  verifyEvidenceContract,
} from "./evidence-contract.js";

test("evidence contract is stable, read-only, unsigned, and content addressed", () => {
  const payload = {
    envelope: {
      schema_version: "1.0",
      kind: "graph",
      summary: "one caller",
      entities: [{ name: "caller", confidence: 1e-7 }],
      evidence: [],
      warnings: [],
    },
  };

  const contract = buildEvidenceContract(payload, {
    producerVersion: "2.6.1",
    transport: "streamable-http",
  });

  assert.equal(contract.schema, LEIO_EVIDENCE_CONTRACT);
  assert.equal(contract.assurance, "unsigned-engineering-evidence");
  assert.equal(contract.effect, "read-only");
  assert.equal(contract.transport, "streamable-http");
  assert.match(contract.payload_sha256, /^[a-f0-9]{64}$/);
  assert.equal(verifyEvidenceContract(payload, contract), true);

  payload.envelope.summary = "tampered";
  assert.equal(verifyEvidenceContract(payload, contract), false);
});

test("canonical digest is independent of object key insertion order", () => {
  const left = { envelope: { kind: "graph", summary: "x" } };
  const right = { envelope: { summary: "x", kind: "graph" } };

  assert.equal(
    buildEvidenceContract(left).payload_sha256,
    buildEvidenceContract(right).payload_sha256,
  );
});
