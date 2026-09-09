#!/usr/bin/env node

import { requireRemoteRepoUrl } from "./repo-url.js";

const repoUrl = process.argv[2] ?? "";

try {
  process.stdout.write(`${requireRemoteRepoUrl(repoUrl)}\n`);
} catch (error) {
  process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
  process.exitCode = 64;
}
