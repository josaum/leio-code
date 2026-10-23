import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import {
  assertNotSymlink,
  encodeWatchRecord,
  forgetSpawn,
  isSymlink,
  maySignalWatchPid,
  newWatchToken,
  openAppendNoFollow,
  parseWatchRecord,
  processLooksLikeBinary,
  recordMatchesSpawn,
  rememberSpawn,
  rememberedSpawn,
  writeExclusiveFile,
} from "./watch-state.js";

test("parseWatchRecord and encodeWatchRecord round-trip and reject invalid records", () => {
  assert.equal(parseWatchRecord(null), null);
  assert.equal(parseWatchRecord("not-a-pid\nabc\n/bin/leio-code"), null);
  assert.equal(parseWatchRecord("0\nabc\n/bin/leio-code"), null);
  assert.equal(parseWatchRecord("12"), null);
  assert.equal(parseWatchRecord("12\n\n/bin/leio-code"), null);
  const encoded = encodeWatchRecord({
    pid: 12,
    token: "abc",
    binaryPath: "/bin/leio-code",
  });
  assert.equal(encoded, "12\nabc\n/bin/leio-code\n");
  assert.deepEqual(parseWatchRecord(encoded), {
    pid: 12,
    token: "abc",
    binaryPath: "/bin/leio-code",
  });
  assert.match(newWatchToken(), /^[a-f0-9]{32}$/);
});

test("writeExclusiveFile, openAppendNoFollow, and isSymlink handle files, symlinks, and errors", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "leio-watch-"));
  const missing = path.join(dir, "missing.pid");
  assert.equal(isSymlink(missing), false);
  assert.throws(() => isSymlink("bad\0path"), /null bytes|ENOENT/i);

  const regularPid = path.join(dir, "regular.pid");
  writeExclusiveFile(regularPid, "12\ntok\n/bin/leio-code\n");
  assert.equal(fs.readFileSync(regularPid, "utf8"), "12\ntok\n/bin/leio-code\n");

  const logFile = path.join(dir, "watch.log");
  const fd = openAppendNoFollow(logFile);
  fs.writeFileSync(fd, "hello\n");
  fs.closeSync(fd);
  assert.equal(fs.readFileSync(logFile, "utf8"), "hello\n");

  const target = path.join(dir, "target");
  const pidPath = path.join(dir, "watch.pid");
  fs.writeFileSync(target, "secret\n");
  fs.symlinkSync(target, pidPath);
  assert.throws(() => assertNotSymlink(pidPath, "watch pid file"), /symlink/);
  assert.throws(() => writeExclusiveFile(pidPath, "1\n"), /symlink/);
  assert.throws(() => openAppendNoFollow(pidPath), /symlink/);
  fs.rmSync(dir, { recursive: true, force: true });
});

test("maySignalWatchPid allows only remembered or identity-checked records", () => {
  assert.equal(recordMatchesSpawn(null, null), false);
  assert.equal(maySignalWatchPid("watch.pid", null), false);
  assert.equal(rememberedSpawn("watch.pid"), null);
  assert.equal(processLooksLikeBinary(999_999_999, ""), false);

  const record = { pid: process.pid, token: "tok", binaryPath: process.execPath };
  assert.equal(maySignalWatchPid("watch.pid", record), true);
  rememberSpawn("watch.pid", record);
  assert.deepEqual(rememberedSpawn("watch.pid"), record);
  assert.equal(maySignalWatchPid("watch.pid", record), true);
  assert.equal(
    maySignalWatchPid("watch.pid", { pid: 1, token: "other", binaryPath: "/nope" }),
    false,
  );
  forgetSpawn("watch.pid");
});
