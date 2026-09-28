import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { applyRegistration } from "./register-mcp.js";

const spec = { command: "/usr/local/bin/node", args: ["/opt/leio/bin/leio-mcp.js"] };

test("registration updates every detected harness and keeps other servers", () => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "leio-mcp-"));
  fs.mkdirSync(path.join(home, ".cursor"));
  fs.writeFileSync(
    path.join(home, ".cursor", "mcp.json"),
    JSON.stringify({ mcpServers: { other: { command: "other" } } }),
  );
  fs.writeFileSync(
    path.join(home, ".claude.json"),
    JSON.stringify({ mcpServers: { kept: { command: "kept" } }, theme: "dark" }),
  );
  fs.mkdirSync(path.join(home, ".codex"));
  fs.writeFileSync(
    path.join(home, ".codex", "config.toml"),
    "[mcp_servers.other]\ncommand = \"other\"\n\n",
  );
  fs.mkdirSync(path.join(home, "Library/Application Support/Code/User"), { recursive: true });
  fs.writeFileSync(
    path.join(home, "Library/Application Support/Code/User/mcp.json"),
    JSON.stringify({ servers: { editor: { command: "editor" } } }),
  );
  fs.mkdirSync(path.join(home, ".gemini"));

  const written = applyRegistration(home, spec);
  assert.ok(written.includes(path.join(home, ".leio-code", "mcp.json")));

  const cursor = JSON.parse(fs.readFileSync(path.join(home, ".cursor/mcp.json"), "utf8"));
  assert.equal(cursor.mcpServers.other.command, "other");
  assert.deepEqual(cursor.mcpServers["leio-code"], spec);

  const claude = JSON.parse(fs.readFileSync(path.join(home, ".claude.json"), "utf8"));
  assert.equal(claude.theme, "dark");
  assert.equal(claude.mcpServers.kept.command, "kept");
  assert.deepEqual(claude.mcpServers["leio-code"], spec);

  const code = JSON.parse(
    fs.readFileSync(path.join(home, "Library/Application Support/Code/User/mcp.json"), "utf8"),
  );
  assert.equal(code.servers.editor.command, "editor");
  assert.equal(code.servers["leio-code"].type, "stdio");
  assert.equal(code.servers["leio-code"].command, spec.command);

  const gemini = JSON.parse(fs.readFileSync(path.join(home, ".gemini/settings.json"), "utf8"));
  assert.deepEqual(gemini.mcpServers["leio-code"], spec);

  const toml = fs.readFileSync(path.join(home, ".codex/config.toml"), "utf8");
  assert.match(toml, /\[mcp_servers\.other\]/);
  assert.match(toml, /\[mcp_servers\.leio-code\]/);
  assert.equal(toml.match(/\[mcp_servers\.leio-code\]/g).length, 1);

  const cursorBefore = fs.readFileSync(path.join(home, ".cursor/mcp.json"));
  const second = applyRegistration(home, spec);
  assert.deepEqual(second, []);
  assert.deepEqual(fs.readFileSync(path.join(home, ".cursor/mcp.json")), cursorBefore);
  const again = fs.readFileSync(path.join(home, ".codex/config.toml"), "utf8");
  assert.equal(again.match(/\[mcp_servers\.leio-code\]/g).length, 1);
});
