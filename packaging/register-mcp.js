import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";

// Harnesses that already have a config directory get a stdio entry.
// The same command/args record is what any other MCP host can launch.
const HOSTS = [
  { dir: ".cursor", file: "mcp.json", kind: "mcpServers" },
  { dir: ".codex", file: "config.toml", kind: "toml" },
  { dir: ".claude", file: "mcp.json", kind: "mcpServers" },
  { dir: ".vscode", file: "mcp.json", kind: "servers" },
  { dir: ".gemini", file: "settings.json", kind: "mcpServers" },
  { dir: ".continue", file: "config.json", kind: "mcpServers" },
  { dir: ".codeium/windsurf", file: "mcp_config.json", kind: "mcpServers" },
  { dir: "Library/Application Support/Claude", file: "claude_desktop_config.json", kind: "mcpServers" },
  { dir: "Library/Application Support/Code/User", file: "mcp.json", kind: "servers" },
  { dir: "Library/Application Support/Cursor/User", file: "mcp.json", kind: "mcpServers" },
  { dir: ".config/Code/User", file: "mcp.json", kind: "servers" },
];

export function serverSpec(command, args) {
  return { command, args };
}

export function applyRegistration(home, spec) {
  const written = [];
  const canonicalDir = path.join(home, ".leio-code");
  fs.mkdirSync(canonicalDir, { recursive: true });
  const canonical = path.join(canonicalDir, "mcp.json");
  const canonicalText = JSON.stringify({ mcpServers: { "leio-code": spec } }, null, 2) + "\n";
  if (!fs.existsSync(canonical) || fs.readFileSync(canonical, "utf8") !== canonicalText) {
    fs.writeFileSync(canonical, canonicalText);
    written.push(canonical);
  }

  const claude = path.join(home, ".claude.json");
  if (fs.existsSync(claude) && mergeJson(claude, spec, "mcpServers")) {
    written.push(claude);
  }

  for (const host of HOSTS) {
    const dir = path.join(home, host.dir);
    if (!fs.existsSync(dir)) continue;
    const file = path.join(dir, host.file);
    if (host.kind === "toml") {
      if (mergeToml(file, spec)) written.push(file);
      continue;
    }
    if (!fs.existsSync(file)) {
      fs.writeFileSync(
        file,
        JSON.stringify(emptyDocument(host.kind, spec), null, 2) + "\n",
      );
      written.push(file);
      continue;
    }
    if (mergeJson(file, spec, host.kind)) written.push(file);
  }
  return written;
}

function emptyDocument(kind, spec) {
  if (kind === "servers") {
    return { servers: { "leio-code": { type: "stdio", ...spec } } };
  }
  return { mcpServers: { "leio-code": spec } };
}

export function mergeJson(file, spec, preferredKind) {
  let data;
  try {
    data = JSON.parse(fs.readFileSync(file, "utf8"));
  } catch {
    return false;
  }
  if (!data || typeof data !== "object" || Array.isArray(data)) return false;
  const useServers = Boolean(data.servers && typeof data.servers === "object" && !data.mcpServers);
  const kind = useServers ? "servers" : preferredKind === "servers" && !data.mcpServers ? "servers" : "mcpServers";
  const bucket = data[kind];
  const next = kind === "servers" ? { type: "stdio", ...spec } : spec;
  if (bucket && typeof bucket === "object" && sameSpec(bucket["leio-code"], next)) return false;
  data[kind] = { ...(bucket && typeof bucket === "object" ? bucket : {}), "leio-code": next };
  fs.writeFileSync(file, JSON.stringify(data, null, 2) + "\n");
  return true;
}

function sameSpec(entry, spec) {
  return Boolean(entry)
    && entry.command === spec.command
    && JSON.stringify(entry.args) === JSON.stringify(spec.args)
    && (spec.type === undefined || entry.type === spec.type);
}

export function mergeToml(file, spec) {
  const block = [
    "[mcp_servers.leio-code]",
    `command = ${JSON.stringify(spec.command)}`,
    `args = ${JSON.stringify(spec.args)}`,
  ];
  const current = fs.existsSync(file) ? fs.readFileSync(file, "utf8") : "";
  if (
    current.includes(`[mcp_servers.leio-code]`)
    && current.includes(`command = ${JSON.stringify(spec.command)}`)
    && current.includes(`args = ${JSON.stringify(spec.args)}`)
  ) {
    return false;
  }
  const lines = current.split("\n");
  const start = lines.findIndex((line) => line.trim() === "[mcp_servers.leio-code]");
  let nextLines;
  if (start === -1) {
    const base = current.replace(/\s*$/, "");
    nextLines = `${base}${base ? "\n\n" : ""}${block.join("\n")}\n`.split("\n");
  } else {
    let end = start + 1;
    while (end < lines.length && !lines[end].startsWith("[")) end += 1;
    nextLines = [...lines.slice(0, start), ...block, ...lines.slice(end)];
  }
  const next = `${nextLines.join("\n").replace(/\n*$/, "")}\n`;
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, next);
  return true;
}

function readSpec() {
  const command = process.env.LEIO_MCP_COMMAND;
  const args = JSON.parse(process.env.LEIO_MCP_ARGS || "[]");
  if (!command || !Array.isArray(args)) {
    throw new Error("LEIO_MCP_COMMAND and LEIO_MCP_ARGS are required");
  }
  return serverSpec(command, args);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const home = process.env.LEIO_MCP_HOME || os.homedir();
  applyRegistration(home, readSpec());
}
