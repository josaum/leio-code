import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));

function runValidateRepoUrl(repoUrl, env = {}) {
  return spawnSync(process.execPath, ["validate-repo-url.mjs", repoUrl], {
    cwd: __dirname,
    encoding: "utf8",
    env: {
      ...process.env,
      ...env,
    },
  });
}

test("validate-repo-url prints the normalized remote URL", () => {
  const result = runValidateRepoUrl("openai/codex");

  assert.equal(result.status, 0);
  assert.equal(result.stdout.trim(), "https://github.com/openai/codex.git");
  assert.equal(result.stderr, "");
});

test("validate-repo-url rejects unsafe fixed-repo clone URLs", () => {
  const result = runValidateRepoUrl("git@github.com:openai/codex.git");

  assert.equal(result.status, 64);
  assert.match(result.stderr, /owner\/repo shorthand or an https URL/);
});

test("validate-repo-url honors deployment host allowlist env", () => {
  const result = runValidateRepoUrl("https://git.example.com/org/repo.git", {
    LEIO_CODE_ALLOWED_REPO_HOSTS: "git.example.com",
  });

  assert.equal(result.status, 0);
  assert.equal(result.stdout.trim(), "https://git.example.com/org/repo.git");
});
