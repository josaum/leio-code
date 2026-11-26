import { chmodSync, existsSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.dirname(fileURLToPath(import.meta.url));
const platform = `${process.platform}-${process.arch}`;
const vendor = path.join(root, "vendor", platform);
const ext = process.platform === "win32" ? ".exe" : "";
const code = path.join(vendor, `leio-code${ext}`);
const harness = path.join(vendor, `leio-harness${ext}`);

if (existsSync(code) && existsSync(harness)) {
  if (process.platform !== "win32") {
    for (const file of [code, harness]) {
      const mode = statSync(file).mode & 0o777;
      // Some archive extractors discard the packaged executable bit.
      if (!(mode & 0o100)) chmodSync(file, mode | 0o100);
    }
  }
  process.env.LEIO_CODE_BIN = code;
  process.env.LEIO_HARNESS_BIN = harness;
}
const git = path.join(root, "runtime", platform, "git", "cmd");
if (existsSync(path.join(git, "git.exe"))) {
  process.env.PATH = [git, process.env.PATH].filter(Boolean).join(path.delimiter);
}
await import("./index.js");
