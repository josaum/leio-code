import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { confineToRepo } from "./export-paths.js";

test("confineToRepo accepts paths inside the repo", () => {
  const root = path.join(os.tmpdir(), "leio-repo");
  assert.equal(
    confineToRepo(root, "exports/out.json"),
    path.resolve(root, "exports/out.json"),
  );
});

test("confineToRepo rejects parent escapes", () => {
  const root = path.join(os.tmpdir(), "leio-repo");
  assert.throws(() => confineToRepo(root, "../outside.json"), /repo_root/);
  assert.throws(() => confineToRepo(root, "/etc/passwd"), /repo_root/);
});

test("confineToRepo rejects a directory symlink that escapes the repo", () => {
  const repo = fs.mkdtempSync(path.join(os.tmpdir(), "leio-repo-"));
  const outside = fs.mkdtempSync(path.join(os.tmpdir(), "leio-out-"));
  fs.symlinkSync(outside, path.join(repo, "exports"));
  assert.throws(() => confineToRepo(repo, "exports/out.json"), /repo_root/);
  fs.rmSync(repo, { recursive: true, force: true });
  fs.rmSync(outside, { recursive: true, force: true });
});
