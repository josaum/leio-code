import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import {
  assertNotSymlink,
  encodeWatchRecord,
  forgetSpawn,
  maySignalWatchPid,
  parseWatchRecord,
  rememberSpawn,
  writeExclusiveFile,
} from "./watch-state.js";

test("parseWatchRecord requires pid token and binary", () => {
  assert.equal(parseWatchRecord("12"), null);
  assert.deepEqual(parseWatchRecord("12\nabc\n/bin/leio-code\n"), {
    pid: 12,
    token: "abc",
    binaryPath: "/bin/leio-code",
  });
});

test("writeExclusiveFile and assertNotSymlink refuse a pid symlink", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "leio-watch-"));
  const target = path.join(dir, "target");
  const pidPath = path.join(dir, "watch.pid");
  fs.writeFileSync(target, "secret\n");
  fs.symlinkSync(target, pidPath);
  assert.throws(() => assertNotSymlink(pidPath, "watch pid file"), /symlink/);
  assert.throws(() => writeExclusiveFile(pidPath, "1\n"), /symlink/);
  fs.rmSync(dir, { recursive: true, force: true });
});

test("maySignalWatchPid allows only remembered or identity-checked records", () => {
  const record = { pid: process.pid, token: "tok", binaryPath: process.execPath };
  assert.equal(maySignalWatchPid("watch.pid", record), true);
  rememberSpawn("watch.pid", record);
  assert.equal(maySignalWatchPid("watch.pid", record), true);
  assert.equal(
    maySignalWatchPid("watch.pid", { pid: 1, token: "other", binaryPath: "/nope" }),
    false,
  );
  forgetSpawn("watch.pid");
});
