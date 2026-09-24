import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import Ajv2020 from "ajv/dist/2020.js";
import addFormats from "ajv-formats";

const schema = JSON.parse(readFileSync(new URL("./contracts/query-result-jsonld-v1.schema.json", import.meta.url)));
const context = JSON.parse(readFileSync(new URL("./contracts/context.jsonld", import.meta.url)))["@context"];
const ajv = new Ajv2020({ strict: true, allErrors: true });
addFormats(ajv);
const validate = ajv.compile(schema);
const fixture = {
  schema_version: "1.0", "@context": context,
  "@id": "urn:leio-code:query:test", "@type": "FindResult",
  query_id: "test", kind: "find", summary: "test", confidence: 1,
  entities: [], evidence: [], warnings: [], timing_ms: 0,
  "http://www.w3.org/1999/02/22-rdf-syntax-ns#type": { "@id": "http://www.w3.org/ns/prov#Activity" },
  "http://www.w3.org/ns/prov#endedAtTime": "2026-09-24T00:00:00Z",
  "http://www.w3.org/ns/prov#wasAssociatedWith": { "@id": "urn:leio-code:agent:test", "@type": "http://www.w3.org/ns/prov#SoftwareAgent" },
};

test("2020-12 producer schema accepts its documented profile", () => {
  assert.equal(validate(fixture), true, JSON.stringify(validate.errors));
});

test("producer schema rejects malformed or falsely advertised documents", () => {
  for (const patch of [
    { "@context": "https://ontology.getjai.com/leio-code/v1#" },
    { "@context": { ...context, "@version": 1.2 } },
    { confidence: 2 }, { timing_ms: -1 }, { evidence: [{}] },
    { "http://www.w3.org/ns/prov#endedAtTime": "yesterday" },
    { "http://www.w3.org/1999/02/22-rdf-syntax-ns#type": true },
  ]) assert.equal(validate({ ...fixture, ...patch }), false, JSON.stringify(patch));
});

test("schema accepts an actual CLI document when provided for verification", {
  skip: !process.env.LEIO_JSONLD_TEST_DOCUMENT,
}, () => {
  const document = JSON.parse(readFileSync(process.env.LEIO_JSONLD_TEST_DOCUMENT, "utf8"));
  assert.equal(validate(document), true, JSON.stringify(validate.errors));
});
