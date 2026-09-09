import test from "node:test";
import assert from "node:assert/strict";

import {
  GUIDE_TOPICS,
  buildGuideStructuredContent,
  filterGuideByCapabilities,
  guideActionPalette,
} from "../mcp/guide.js";
import { STDIO_TOOL_CATALOG } from "./mcp-spec-2025-11-25.js";

const genericCapabilities = {
  workspace_profile: "generic",
  find_kinds: ["symbol", "env-var", "redis-key", "api-route", "docker-service"],
  explain_kinds: ["env-var", "redis-key"],
  doctor_kinds: [],
};

const leioCapabilities = {
  ...genericCapabilities,
  workspace_profile: "leio-code",
  doctor_kinds: ["self-contract", "slop", "import-boundary", "repo-hygiene"],
};

test("guide topics include graph, navigation and doctor routing", () => {
  assert.ok(GUIDE_TOPICS.includes("graph"));
  assert.ok(GUIDE_TOPICS.includes("navigation"));
  assert.ok(GUIDE_TOPICS.includes("doctor"));
  const built = buildGuideStructuredContent("graph");
  assert.match(built.text, /callers-of/);
  assert.equal(built.structuredContent.topic, "graph");
});

test("guide recommendations match the topic and advertised stdio tools", () => {
  const firstTools = {
    general: "leio_code_capabilities", context: "leio_code_context",
    lookup: "leio_code_find", explain: "leio_code_explain",
    doctor: "leio_code_doctor", graph: "leio_code_graph",
    navigation: "leio_code_nav",
    export: "leio_code_export", knowledge: "leio_code_knowledge",
    conversation: "leio_code_conversation",
  };
  for (const [topic, first] of Object.entries(firstTools)) {
    const built = buildGuideStructuredContent(topic, { capabilities: leioCapabilities });
    assert.equal(built.structuredContent.next_tools[0], first, topic);
    assert.match(built.text, new RegExp(first));
    assert.ok(built.structuredContent.next_tools.length <= 3);
    for (const tool of built.structuredContent.next_tools) assert.ok(STDIO_TOOL_CATALOG[tool], tool);
  }
  const generic = buildGuideStructuredContent("doctor", { capabilities: genericCapabilities });
  assert.deepEqual(generic.structuredContent.next_tools, ["leio_code_capabilities"]);
  for (const topic of ["unknown", "__proto__", "constructor"]) {
    assert.deepEqual(buildGuideStructuredContent(topic).structuredContent.next_tools,
      buildGuideStructuredContent("general").structuredContent.next_tools);
  }
});

test("graph and navigation guidance separate structural evidence from FCA exploration", () => {
  const graph = buildGuideStructuredContent("graph");
  assert.deepEqual(graph.structuredContent.next_tools,
    ["leio_code_graph", "leio_code_find", "leio_code_nav"]);
  assert.match(graph.text, /stable.*URN/i);
  assert.match(graph.text, /formal-context.*not required/i);
  const navigation = buildGuideStructuredContent("navigation");
  assert.deepEqual(navigation.structuredContent.next_tools,
    ["leio_code_nav", "leio_code_graph", "leio_code_export"]);
  for (const contract of [
    /parent.*more general/, /child.*more specific/, /peer.*shared parent/,
    /zero-based/, /select.*before.*walk/, /shared indexed attributes/,
    /not proof of runtime coupling/, /LEIO_SESSION/, /hosted.*no nav tool/i,
  ]) assert.match(navigation.text, contract);
  const hosted = buildGuideStructuredContent("navigation", { nextTools: ["graph_repository"] });
  assert.deepEqual(hosted.structuredContent.next_tools, ["graph_repository"]);
});

test("navigation examples pin the repository and session across selection and history", () => {
  const { structuredContent: guide } = buildGuideStructuredContent("navigation");
  const calls = guide.examples.map((example) => {
    const [, tool, json] = example.match(/^(leio_code_[a-z_]+)\((.*)\)$/);
    assert.ok(STDIO_TOOL_CATALOG[tool], tool);
    return { tool, arguments: JSON.parse(json) };
  });
  const navigation = calls.filter((call) => call.tool === "leio_code_nav");
  assert.deepEqual(navigation.map((call) => call.arguments.kind), ["goto", "parent", "select", "back"]);
  for (const call of calls) assert.equal(call.arguments.repo_root, "/abs/repo");
  for (const call of navigation) assert.equal(call.arguments.session, "agent-auth-review");
  assert.equal(navigation[2].arguments.index, 0);
  assert.equal(navigation[2].arguments.select, undefined);
});

test("guide action palette follows the topic without mutating global choices", () => {
  const palette = { recommended_start_tool: "capabilities", recommended_next_tools: ["capabilities"], show_tools: ["capabilities"], hide_tools: ["doctor"] };
  const guided = guideActionPalette(palette, ["graph"]);
  assert.equal(guided.recommended_start_tool, "graph");
  assert.deepEqual(guided.recommended_next_tools, ["graph"]);
  assert.deepEqual(guided.show_tools, ["capabilities", "graph"]);
  assert.deepEqual(guided.hide_tools, ["doctor"]);
  assert.equal(palette.recommended_start_tool, "capabilities");
  assert.equal(guideActionPalette(palette, []).recommended_start_tool, undefined);
  assert.deepEqual(guideActionPalette(palette, []).recommended_next_tools, []);
  assert.equal(guideActionPalette(null, ["graph"]), null);
});

test("knowledge guide covers the local compiled wiki", () => {
  const built = buildGuideStructuredContent("knowledge");
  assert.match(built.text, /knowledge/);
  assert.equal(built.structuredContent.topic, "knowledge");
  assert.ok(Array.isArray(built.structuredContent.next_tools));
});

test("default routing_doc is the canonical skill", () => {
  const built = buildGuideStructuredContent("general");
  assert.equal(
    built.structuredContent.routing_doc,
    "skills/leio-code/SKILL.md",
  );
});

test("explicit routingDoc override is preserved", () => {
  const built = buildGuideStructuredContent("general", {
    routingDoc: "docs/custom.md",
  });
  assert.equal(built.structuredContent.routing_doc, "docs/custom.md");
});

test("filterGuideByCapabilities drops deploy-target lines on generic profiles", () => {
  const guide = {
    when: "lookup",
    tools: [
      "find kind=symbol",
      "find kind=deploy-target for topology",
      "run doctor all",
    ],
    examples: ["Find symbol X.", "Find deploy-target backend_api."],
  };
  const filtered = filterGuideByCapabilities(guide, {
    find_kinds: ["symbol", "env-var"],
    explain_kinds: ["env-var"],
    doctor_kinds: ["env-contract"],
  });
  assert.deepEqual(filtered.tools, ["find kind=symbol", "run doctor all"]);
  assert.deepEqual(filtered.examples, ["Find symbol X."]);
});

for (const capabilities of [genericCapabilities, leioCapabilities]) {
  test(`${capabilities.workspace_profile} general guide preserves core tool routing`, () => {
    const { structuredContent: guide } = buildGuideStructuredContent("general", {
      capabilities,
    });
    for (const route of [
      "status / inspect_repository_status",
      "capabilities / inspect_repository_capabilities",
      "context / prepare_repository_context",
      "find / search_repository for",
      "explain / explain_repository for",
      "graph / graph_repository",
    ]) {
      assert.ok(guide.tools.some((line) => line.startsWith(route)), route);
    }
    assert.doesNotMatch(guide.tools.join("\n"), /deploy-target|cartridge/);
    assert.doesNotMatch(guide.examples.join("\n"), /Run doctor deploy/);
  });

  test(`${capabilities.workspace_profile} context guide retains its bundle introduction`, () => {
    const { structuredContent: guide } = buildGuideStructuredContent("context", {
      capabilities,
    });
    assert.match(guide.tools[0], /^context \/ prepare_repository_context\(task=/);
    for (const field of ["ranked files", "graph follow-ups", "tests", "anchors", "memory sources", "instruction sources", "risk notes"]) {
      assert.ok(guide.tools[0].includes(field), field);
    }
    assert.doesNotMatch(guide.tools.join("\n"), /deploy-target|cartridge/);
  });
}

test("doctor-less generic guide removes doctor commands from real topics", () => {
  for (const topic of ["general", "context", "doctor"]) {
    const { structuredContent: guide } = buildGuideStructuredContent(topic, {
      capabilities: genericCapabilities,
    });
    assert.doesNotMatch(
      [...guide.tools, ...guide.examples].join("\n"),
      /\bdoctor(?:\s+\/\s+audit_repository_contracts)?\s+(?:kind=|all|baseline|ci|deploy|auth-brokering)/i,
      topic,
    );
  }
});

test("LEIO doctor guide preserves presets and omits unsupported concrete suites", () => {
  const { structuredContent: guide } = buildGuideStructuredContent("doctor", {
    capabilities: leioCapabilities,
  });
  for (const preset of ["all", "baseline", "ci"]) {
    assert.ok(guide.examples.includes(`Run doctor ${preset}.`), preset);
  }
  assert.doesNotMatch(guide.examples.join("\n"), /doctor (?:deploy|auth-brokering)/);
  assert.ok(guide.tools.some((line) => line.includes("Targeted doctor kinds")));
});

test("workspace guide retains supported deploy, cartridge and doctor instructions", () => {
  const capabilities = {
    ...genericCapabilities,
    workspace_profile: "workspace",
    find_kinds: [...genericCapabilities.find_kinds, "deploy-target", "cartridge"],
    explain_kinds: [...genericCapabilities.explain_kinds, "deploy-target", "cartridge"],
    doctor_kinds: ["deploy", "auth-brokering"],
  };
  for (const topic of ["general", "lookup", "explain"]) {
    const { structuredContent: guide } = buildGuideStructuredContent(topic, {
      capabilities,
    });
    assert.match(guide.tools.join("\n"), /deploy-target/, topic);
    assert.match(guide.tools.join("\n"), /cartridge/, topic);
    assert.match(guide.examples.join("\n"), /deploy-target/, topic);
    assert.match(guide.examples.join("\n"), /cartridge/, topic);
  }
  const { structuredContent: doctor } = buildGuideStructuredContent("doctor", {
    capabilities,
  });
  assert.ok(doctor.examples.includes("Run doctor deploy."));
  assert.ok(doctor.examples.includes("Run doctor auth-brokering."));
});

test("doctor command filtering handles stdio, hosted and CLI syntax without filtering presets", () => {
  const filtered = filterGuideByCapabilities({
    when: "doctor",
    tools: [
      "doctor kind=self-contract",
      "doctor / audit_repository_contracts kind=baseline",
      "doctor / audit_repository_contracts kind=auth-brokering",
      "doctor ci",
      "Targeted doctor kinds from capabilities / doctor --help.",
    ],
    examples: ["Run doctor all.", "Shell: leio-code doctor deploy."],
  }, leioCapabilities);
  assert.deepEqual(filtered.tools, [
    "doctor kind=self-contract",
    "doctor / audit_repository_contracts kind=baseline",
    "doctor ci",
    "Targeted doctor kinds from capabilities / doctor --help.",
  ]);
  assert.deepEqual(filtered.examples, ["Run doctor all."]);
});
