import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
  applyRegistration,
  installDefaultSkill,
  mergeJson,
  mergeToml,
  resolveDefaultSkillSource,
  serverSpec,
} from "./register-mcp.js";

const MODULE_PATH = fileURLToPath(new URL("./register-mcp.js", import.meta.url));
const spec = serverSpec("/usr/local/bin/node", ["/opt/leio/bin/leio-mcp.js"]);

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
  fs.mkdirSync(path.join(home, "AppData/Roaming/Code/User"), { recursive: true });
  fs.writeFileSync(
    path.join(home, "AppData/Roaming/Code/User/mcp.json"),
    JSON.stringify({ servers: { winEditor: { command: "win-editor" } } }),
  );
  fs.mkdirSync(path.join(home, ".vscode"));
  fs.mkdirSync(path.join(home, ".gemini/antigravity"), { recursive: true });
  fs.writeFileSync(
    path.join(home, ".gemini/antigravity/mcp_config.json"),
    JSON.stringify({
      mcpServers: {
        "leio-code": { ...spec, disabled: true },
        studio: { command: "studio-mcp" },
      },
    }),
  );

  const written = applyRegistration(home, spec);
  assert.ok(written.includes(path.join(home, ".leio-code", "mcp.json")));
  assert.ok(written.includes(path.join(home, ".vscode", "mcp.json")));
  assert.ok(written.includes(path.join(home, ".gemini/antigravity/mcp_config.json")));

  const vscode = JSON.parse(fs.readFileSync(path.join(home, ".vscode/mcp.json"), "utf8"));
  assert.equal(vscode.servers["leio-code"].type, "stdio");
  assert.equal(vscode.servers["leio-code"].command, spec.command);

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

  const winCode = JSON.parse(
    fs.readFileSync(path.join(home, "AppData/Roaming/Code/User/mcp.json"), "utf8"),
  );
  assert.equal(winCode.servers.winEditor.command, "win-editor");
  assert.equal(winCode.servers["leio-code"].type, "stdio");
  assert.equal(winCode.servers["leio-code"].command, spec.command);

  const gemini = JSON.parse(fs.readFileSync(path.join(home, ".gemini/settings.json"), "utf8"));
  assert.deepEqual(gemini.mcpServers["leio-code"], spec);
  const antigravity = JSON.parse(
    fs.readFileSync(path.join(home, ".gemini/antigravity/mcp_config.json"), "utf8"),
  );
  assert.equal(antigravity.mcpServers.studio.command, "studio-mcp");
  assert.deepEqual(antigravity.mcpServers["leio-code"], spec);
  assert.equal(antigravity.mcpServers["leio-code"].disabled, undefined);
  assert.ok(fs.existsSync(path.join(home, ".leio-code/skills/leio-code/SKILL.md")));
  assert.ok(fs.existsSync(path.join(home, ".gemini/config/skills/leio-code/SKILL.md")));
  assert.ok(fs.existsSync(path.join(home, ".codex/skills/leio-code/SKILL.md")));
  assert.ok(fs.existsSync(path.join(home, ".cursor/skills/leio-code/SKILL.md")));

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

test("mergeJson, mergeToml, and skill sync handle malformed inputs and updates", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "leio-mcp-edge-"));
  const badJson = path.join(dir, "bad.json");
  fs.writeFileSync(badJson, "{not-json");
  assert.equal(mergeJson(badJson, spec, "mcpServers"), false);

  const arrayJson = path.join(dir, "array.json");
  fs.writeFileSync(arrayJson, "[]");
  assert.equal(mergeJson(arrayJson, spec, "mcpServers"), false);

  const emptyServersJson = path.join(dir, "vscode.json");
  fs.writeFileSync(emptyServersJson, "{}");
  assert.equal(mergeJson(emptyServersJson, spec, "servers"), true);
  assert.equal(mergeJson(emptyServersJson, spec, "servers"), false);

  const tomlFile = path.join(dir, "config.toml");
  assert.equal(mergeToml(tomlFile, { command: "old-bin", args: ["a"] }), true);
  fs.appendFileSync(tomlFile, "\n[other_section]\nkey = \"val\"\n");
  assert.equal(mergeToml(tomlFile, spec), true);
  const updatedToml = fs.readFileSync(tomlFile, "utf8");
  assert.match(updatedToml, /\/usr\/local\/bin\/node/);
  assert.match(updatedToml, /\[other_section\]/);
  assert.equal(mergeToml(tomlFile, spec), false);

  assert.equal(resolveDefaultSkillSource(path.join(dir, "missing-skill")), null);
  assert.deepEqual(installDefaultSkill(dir, path.join(dir, "missing-skill")), []);

  const customSkill = path.join(dir, "custom-skill");
  fs.mkdirSync(customSkill, { recursive: true });
  fs.writeFileSync(path.join(customSkill, "SKILL.md"), "---\nname: leio-code\n---\n");
  fs.writeFileSync(path.join(customSkill, ".hidden"), "ignored");
  fs.symlinkSync(path.join(customSkill, "SKILL.md"), path.join(customSkill, "link.md"));
  const home = path.join(dir, "home");
  fs.mkdirSync(home, { recursive: true });
  const installed = installDefaultSkill(home, customSkill);
  assert.equal(installed.length, 1);
  assert.equal(fs.existsSync(path.join(home, ".leio-code/skills/leio-code/.hidden")), false);
  assert.equal(fs.existsSync(path.join(home, ".leio-code/skills/leio-code/link.md")), false);

  const noSkillHome = path.join(dir, "no-skill-home");
  fs.mkdirSync(noSkillHome, { recursive: true });
  applyRegistration(noSkillHome, spec, { installSkill: false });
  assert.equal(fs.existsSync(path.join(noSkillHome, ".leio-code/skills")), false);
});

test("CLI entrypoint registers from environment and rejects missing command", () => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "leio-mcp-cli-"));
  execFileSync(process.execPath, [MODULE_PATH], {
    env: {
      ...process.env,
      LEIO_MCP_HOME: home,
      LEIO_MCP_COMMAND: "/usr/local/bin/node",
      LEIO_MCP_ARGS: JSON.stringify(["/opt/leio/mcp/index.js"]),
    },
  });
  assert.ok(fs.existsSync(path.join(home, ".leio-code", "mcp.json")));

  assert.throws(() => {
    execFileSync(process.execPath, [MODULE_PATH], {
      env: { ...process.env, LEIO_MCP_HOME: home, LEIO_MCP_COMMAND: "" },
      stdio: "pipe",
    });
  }, /LEIO_MCP_COMMAND and LEIO_MCP_ARGS are required/);
});
