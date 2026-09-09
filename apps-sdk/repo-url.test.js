import test from "node:test";
import assert from "node:assert/strict";

import {
  isGitHubRepoUrl,
  normalizeRepoUrl,
  parseGitHubOwnerRepo,
  requireRemoteRepoUrl,
} from "./repo-url.js";

test("normalizeRepoUrl accepts GitHub shorthand", () => {
  assert.equal(
    normalizeRepoUrl("openai/codex"),
    "https://github.com/openai/codex.git",
  );
  assert.equal(
    normalizeRepoUrl("openai/codex.git"),
    "https://github.com/openai/codex.git",
  );
});

test("normalizeRepoUrl accepts allowed https remotes", () => {
  assert.equal(
    normalizeRepoUrl("https://github.com/OpenAI/codex.git"),
    "https://github.com/OpenAI/codex.git",
  );
  assert.equal(
    normalizeRepoUrl("https://gitlab.com/group/subgroup/project.git/"),
    "https://gitlab.com/group/subgroup/project.git",
  );
});

test("normalizeRepoUrl rejects non-remote or unsafe clone strings", () => {
  for (const value of [
    "",
    "/tmp/repo",
    "../repo",
    "git@github.com:openai/codex.git",
    "ssh://github.com/openai/codex.git",
    "git://github.com/openai/codex.git",
    "file:///tmp/repo",
    "https://github.com/openai/codex git",
    "https://token@github.com/openai/codex.git",
    "https://github.com/openai/codex.git?depth=1",
    "https://github.com/openai/codex.git#main",
    "https://github.com/openai/../codex.git",
    "https://github.com/openai/%2e%2e/codex.git",
    "https://github.com/openai/%2fcodex.git",
  ]) {
    assert.equal(normalizeRepoUrl(value), null, value);
  }
});

test("requireRemoteRepoUrl explains rejected repo URLs", () => {
  assert.throws(
    () => requireRemoteRepoUrl("git@github.com:openai/codex.git"),
    /owner\/repo shorthand or an https URL/,
  );
  assert.throws(
    () => requireRemoteRepoUrl("ssh://github.com/openai/codex.git"),
    /must use https/,
  );
});

test("normalizeRepoUrl enforces the configured host allowlist", () => {
  assert.equal(
    normalizeRepoUrl("https://example.com/org/repo.git"),
    null,
  );
  assert.equal(
    normalizeRepoUrl("https://example.com/org/repo.git", {
      env: { LEIO_CODE_ALLOWED_REPO_HOSTS: "example.com" },
    }),
    "https://example.com/org/repo.git",
  );
  assert.equal(
    normalizeRepoUrl("https://anything.invalid/org/repo.git", {
      env: { LEIO_CODE_ALLOWED_REPO_HOSTS: "*" },
    }),
    "https://anything.invalid/org/repo.git",
  );
});

test("normalizeRepoUrl only allows localhost http when explicitly enabled", () => {
  assert.equal(normalizeRepoUrl("http://localhost/org/repo.git"), null);
  assert.equal(
    normalizeRepoUrl("http://localhost/org/repo.git", {
      allowInsecureLocalhost: true,
    }),
    "http://localhost/org/repo.git",
  );
});

test("GitHub helpers parse normalized repository identity", () => {
  assert.equal(isGitHubRepoUrl("openai/codex"), true);
  assert.equal(isGitHubRepoUrl("https://gitlab.com/openai/codex.git"), false);
  assert.deepEqual(parseGitHubOwnerRepo("https://github.com/openai/codex"), {
    owner: "openai",
    repo: "codex",
  });
});
