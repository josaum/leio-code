import { execFileSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const spawned = new Map();

export function isSymlink(filePath) {
  try {
    return fs.lstatSync(filePath).isSymbolicLink();
  } catch (error) {
    if (error && error.code === "ENOENT") {
      return false;
    }
    throw error;
  }
}

export function assertNotSymlink(filePath, label = filePath) {
  if (isSymlink(filePath)) {
    throw new Error(`${label} is a symlink`);
  }
}

export function encodeWatchRecord({ pid, token, binaryPath }) {
  return `${pid}\n${token}\n${binaryPath}\n`;
}

export function parseWatchRecord(raw) {
  const lines = String(raw ?? "")
    .trim()
    .split("\n");
  const pid = Number.parseInt(lines[0] ?? "", 10);
  if (!Number.isFinite(pid) || pid <= 0) {
    return null;
  }
  const token = lines[1] ?? "";
  const binaryPath = lines[2] ?? "";
  if (!token || !binaryPath) {
    return null;
  }
  return { pid, token, binaryPath };
}

export function newWatchToken() {
  return crypto.randomBytes(16).toString("hex");
}

export function writeExclusiveFile(filePath, contents) {
  assertNotSymlink(filePath);
  const fd = fs.openSync(
    filePath,
    fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_EXCL,
  );
  try {
    fs.writeFileSync(fd, contents);
  } finally {
    fs.closeSync(fd);
  }
}

export function openAppendNoFollow(filePath) {
  assertNotSymlink(filePath);
  const nofollow = fs.constants.O_NOFOLLOW ?? 0;
  return fs.openSync(
    filePath,
    fs.constants.O_WRONLY |
      fs.constants.O_CREAT |
      fs.constants.O_APPEND |
      nofollow,
  );
}

export function rememberSpawn(key, record) {
  spawned.set(key, record);
}

export function forgetSpawn(key) {
  spawned.delete(key);
}

export function rememberedSpawn(key) {
  return spawned.get(key) ?? null;
}

export function processLooksLikeBinary(pid, binaryPath) {
  const base = path.basename(binaryPath || "leio-code");
  let cmd = "";
  try {
    cmd = fs.readFileSync(`/proc/${pid}/cmdline`, "utf8").replace(/\0/g, " ");
  } catch {
    try {
      cmd = execFileSync("ps", ["-p", String(pid), "-o", "args="], {
        encoding: "utf8",
        timeout: 1000,
      });
    } catch {
      return false;
    }
  }
  return cmd.includes(base) || cmd.includes("leio-code");
}

export function recordMatchesSpawn(record, remembered) {
  if (!record || !remembered) {
    return false;
  }
  return (
    record.pid === remembered.pid &&
    record.token === remembered.token &&
    record.binaryPath === remembered.binaryPath
  );
}

export function maySignalWatchPid(key, record) {
  if (!record) {
    return false;
  }
  const remembered = rememberedSpawn(key);
  if (recordMatchesSpawn(record, remembered)) {
    return true;
  }
  return Boolean(record.token) && processLooksLikeBinary(record.pid, record.binaryPath);
}
