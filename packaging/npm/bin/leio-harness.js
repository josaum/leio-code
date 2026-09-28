#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const binary = path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "vendor", "leio-harness");
try {
  execFileSync(binary, process.argv.slice(2), { stdio: "inherit" });
} catch (error) {
  process.exit(error.status ?? 1);
}
