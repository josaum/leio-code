import { createHash } from "node:crypto";

export const LEIO_EVIDENCE_CONTRACT =
  "urn:leio-code:unsigned-engineering-evidence:v1";

function canonicalize(value) {
  if (Array.isArray(value)) {
    return value.map(canonicalize);
  }
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.keys(value)
        .sort()
        .filter((key) => key !== "evidence_contract")
        .map((key) => [key, canonicalize(value[key])]),
    );
  }
  if (typeof value === "number") {
    if (!Number.isFinite(value)) {
      throw new TypeError("evidence contract cannot canonicalize non-finite numbers");
    }
    const bytes = new Uint8Array(8);
    new DataView(bytes.buffer).setFloat64(0, value, false);
    return `@f64:${Buffer.from(bytes).toString("hex")}`;
  }
  return value;
}

export function canonicalEvidenceJson(value) {
  return JSON.stringify(canonicalize(value));
}

export function evidencePayloadDigest(value) {
  return createHash("sha256").update(canonicalEvidenceJson(value)).digest("hex");
}

export function buildEvidenceContract(
  structuredContent,
  { producerVersion = null, transport = "stdio" } = {},
) {
  return {
    schema: LEIO_EVIDENCE_CONTRACT,
    version: "1.0",
    assurance: "unsigned-engineering-evidence",
    effect: "read-only",
    transport,
    producer: {
      name: "leio-code",
      version: producerVersion,
    },
    payload_sha256: evidencePayloadDigest(structuredContent),
  };
}

export function verifyEvidenceContract(structuredContent, contract) {
  return (
    contract?.schema === LEIO_EVIDENCE_CONTRACT &&
    contract?.version === "1.0" &&
    contract?.assurance === "unsigned-engineering-evidence" &&
    contract?.effect === "read-only" &&
    contract?.payload_sha256 === evidencePayloadDigest(structuredContent)
  );
}
