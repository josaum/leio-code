import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";

import {
  findBinaryUpwards,
  findInstalledBinary,
  isCargoTargetBinary,
  isTrustedEnvBinary,
  resolveBinaryPath,
} from "./resolve-binary.js";

test("finds the binary in a PARENT target/ when leio-code/target is absent (shared CARGO_TARGET_DIR regression)", () => {
  const workspace = "/ws";
  const leioRoot = path.join(workspace, "leio-code");
  const expected = path.join(workspace, "target", "debug", "leio-code");
  const present = new Set([expected]);

  assert.equal(
    findBinaryUpwards(leioRoot, "leio-code", (p) => present.has(p), {
      homeDir: "/Users/test",
    }),
    expected,
  );
});

test("prefers a release build over a debug build in the same dir", () => {
  const root = "/ws/leio-code";
  const release = path.join(root, "target", "release", "leio-code");
  const debug = path.join(root, "target", "debug", "leio-code");
  const present = new Set([release, debug]);

  assert.equal(
    findBinaryUpwards(root, "leio-code", (p) => present.has(p), {
      homeDir: "/Users/test",
    }),
    release,
  );
});

test("returns null when no binary exists up to the filesystem root", () => {
  assert.equal(
    findBinaryUpwards("/ws/leio-code", "leio-code", () => false, {
      homeDir: "/Users/test",
    }),
    null,
  );
});

test("findBinaryUpwards does not inspect $HOME/target", () => {
  const home = "/Users/test";
  const planted = path.join(home, "target", "release", "leio-code");
  const present = new Set([planted]);
  assert.equal(
    findBinaryUpwards(path.join(home, "projects", "app"), "leio-code", (p) => present.has(p), {
      homeDir: home,
    }),
    null,
  );
});

test("finds a trusted Cargo-installed binary when the MCP package runs from a plugin cache", () => {
  const homeDir = "/Users/test";
  const expected = path.join(homeDir, ".cargo", "bin", "leio-code");
  const present = new Set([expected]);

  assert.equal(
    findInstalledBinary("leio-code", {
      homeDir,
      pathValue: "/opt/homebrew/bin:/usr/bin:/bin",
      exists: (candidate) => present.has(candidate),
    }),
    expected,
  );
});

test("findInstalledBinary skips a PATH entry that is a cargo target dir", () => {
  const target = "/ws/target/release/leio-code";
  assert.equal(isCargoTargetBinary(target), true);
  assert.equal(
    findInstalledBinary("leio-code", {
      homeDir: "/Users/test",
      pathValue: "/ws/target/release:/usr/bin",
      exists: (candidate) => candidate === target,
    }),
    null,
  );
});

test("findInstalledBinary skips target/debug and prefers a later PATH install", () => {
  const debugTarget = "/ws/target/debug/leio-code";
  const installed = "/opt/bin/leio-code";
  const present = new Set([debugTarget, installed]);
  assert.equal(
    findInstalledBinary("leio-code", {
      homeDir: "/Users/test",
      pathValue: "/ws/target/debug:/opt/bin",
      exists: (candidate) => present.has(candidate),
    }),
    installed,
  );
});

test("findInstalledBinary skips rustc target triples under target/", () => {
  const triple = "/ws/target/x86_64-apple-darwin/release/leio-code";
  assert.equal(isCargoTargetBinary(triple), true);
  assert.equal(
    findInstalledBinary("leio-code", {
      homeDir: "/Users/test",
      pathValue: "/ws/target/x86_64-apple-darwin/release",
      exists: (candidate) => candidate === triple,
    }),
    null,
  );
});

test("findInstalledBinary skips CARGO_TARGET_DIR release entries", () => {
  const custom = "/tmp/leio-out/release/leio-code";
  assert.equal(
    isCargoTargetBinary(custom, { cargoTargetDir: "/tmp/leio-out" }),
    true,
  );
  assert.equal(
    findInstalledBinary("leio-code", {
      homeDir: "/Users/test",
      pathValue: "/tmp/leio-out/release:/opt/bin",
      cargoTargetDir: "/tmp/leio-out",
      exists: (candidate) => candidate === custom,
    }),
    null,
  );
});

test("findInstalledBinary honors CARGO_HOME", () => {
  const expected = "/opt/custom-cargo/bin/leio-code";
  assert.equal(
    findInstalledBinary("leio-code", {
      cargoHome: "/opt/custom-cargo",
      homeDir: "/Users/test",
      pathValue: "/usr/bin",
      exists: (candidate) => candidate === expected,
    }),
    expected,
  );
});

test("resolveBinaryPath prefers LEIO_CODE_BIN over cargo bin and target/", () => {
  const envPath = "/opt/explicit/leio-code";
  const cargoBin = "/Users/test/.cargo/bin/leio-code";
  const target = "/ws/target/release/leio-code";
  const present = new Set([envPath, cargoBin, target]);

  assert.equal(
    resolveBinaryPath({
      binaryName: "leio-code",
      startDir: "/ws/leio-code",
      envPath,
      homeDir: "/Users/test",
      pathValue: "/usr/bin",
      exists: (candidate) => present.has(candidate),
    }),
    path.resolve(envPath),
  );
});

test("resolveBinaryPath prefers cargo bin over any target/ walk-up", () => {
  const cargoBin = "/Users/test/.cargo/bin/leio-code";
  const target = "/ws/target/release/leio-code";
  const present = new Set([cargoBin, target]);

  assert.equal(
    resolveBinaryPath({
      binaryName: "leio-code",
      startDir: "/ws/leio-code",
      envPath: "",
      homeDir: "/Users/test",
      pathValue: "/usr/bin",
      exists: (candidate) => present.has(candidate),
    }),
    cargoBin,
  );
});

test("resolveBinaryPath uses target/ only when no installed binary exists", () => {
  const target = "/ws/target/debug/leio-code";
  const present = new Set([target]);

  assert.equal(
    resolveBinaryPath({
      binaryName: "leio-code",
      startDir: "/ws/leio-code",
      envPath: "",
      homeDir: "/Users/test",
      pathValue: "/usr/bin",
      exists: (candidate) => present.has(candidate),
    }),
    target,
  );
});

test("resolveBinaryPath mixed PATH prefers install over target via helper", () => {
  const target = "/ws/target/release/leio-code";
  const installed = "/opt/bin/leio-code";
  const present = new Set([target, installed]);
  assert.equal(
    resolveBinaryPath({
      binaryName: "leio-code",
      startDir: "/ws/leio-code",
      envPath: "",
      homeDir: "/Users/test",
      pathValue: "/ws/target/release:/opt/bin",
      exists: (candidate) => present.has(candidate),
    }),
    installed,
  );
});

test("resolveBinaryPath ignores LEIO_CODE_BIN when exists is false", () => {
  const cargoBin = "/Users/test/.cargo/bin/leio-code";
  assert.equal(
    resolveBinaryPath({
      binaryName: "leio-code",
      startDir: "/ws/leio-code",
      envPath: "/missing/leio-code",
      homeDir: "/Users/test",
      pathValue: "/usr/bin",
      exists: (candidate) => candidate === cargoBin,
    }),
    cargoBin,
  );
});

test("mcp/index.js imports the shared resolver", () => {
  const src = readFileSync(new URL("./index.js", import.meta.url), "utf8");
  assert.match(src, /from ["']\.\/resolve-binary\.js["']/);
});

test("isTrustedEnvBinary rejects directories and wrong basenames", () => {
  assert.equal(
    isTrustedEnvBinary("/opt/explicit/leio-code", {
      exists: () => true,
      stat: () => ({ isFile: () => true, isDirectory: () => false }),
    }),
    true,
  );
  assert.equal(
    isTrustedEnvBinary("/opt/explicit/leio-code", {
      exists: () => true,
      stat: () => ({ isFile: () => false, isDirectory: () => true }),
    }),
    false,
  );
  assert.equal(
    isTrustedEnvBinary("/tmp/not-leio", {
      exists: () => true,
      stat: () => ({ isFile: () => true, isDirectory: () => false }),
    }),
    false,
  );
});
