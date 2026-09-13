import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const TARGET_PROFILE = /(?:^|\/)(?:release|debug)(?:\/|$)/;

/**
 * Cargo artifacts: `target/{release,debug}`, `target/<triple>/{release,debug}`,
 * or `$CARGO_TARGET_DIR/{release,debug}`.
 */
export function isCargoTargetBinary(
  candidate,
  { cargoTargetDir = process.env.CARGO_TARGET_DIR ?? "" } = {},
) {
  const normalized = String(candidate ?? "").split(path.sep).join("/");
  if (/\/target\/(?:[^/]+\/)?(?:release|debug)(?:\/|$)/.test(normalized)) {
    return true;
  }
  const targetDir = String(cargoTargetDir ?? "").trim();
  if (targetDir) {
    const prefix = path.resolve(targetDir).split(path.sep).join("/");
    if (normalized === prefix || normalized.startsWith(`${prefix}/`)) {
      return TARGET_PROFILE.test(normalized.slice(prefix.length));
    }
  }
  return false;
}

function defaultStat(candidate) {
  try {
    return fs.statSync(candidate);
  } catch {
    return null;
  }
}

export function isTrustedEnvBinary(
  candidate,
  {
    exists = fs.existsSync,
    stat = defaultStat,
  } = {},
) {
  const trimmed = String(candidate ?? "").trim();
  if (!trimmed) {
    return false;
  }
  const resolved = path.resolve(trimmed);
  const base = path.basename(resolved);
  if (base !== "leio-code" && base !== "leio-code.exe") {
    return false;
  }
  if (!exists(resolved)) {
    return false;
  }
  const info = stat(resolved);
  if (info) {
    if (typeof info.isDirectory === "function" && info.isDirectory()) {
      return false;
    }
    if (typeof info.isFile === "function" && !info.isFile()) {
      return false;
    }
  }
  return true;
}

/**
 * Walk up from `startDir` looking for a built leio-code binary at
 * `<dir>/target/{release,debug}/<binaryName>`, returning the first match.
 *
 * Stops before `$HOME` and the filesystem root so a planted
 * `~/target/release/leio-code` or `/target/release/leio-code` cannot win.
 *
 * `exists` is injectable for testing; defaults to `fs.existsSync`.
 */
export function findBinaryUpwards(
  startDir,
  binaryName,
  exists = fs.existsSync,
  { homeDir = os.homedir() } = {},
) {
  let current = path.resolve(startDir);
  const home = homeDir ? path.resolve(homeDir) : null;
  const root = path.parse(current).root;
  while (true) {
    if (current === root || (home && current === home)) {
      return null;
    }
    for (const profile of ["release", "debug"]) {
      const candidate = path.join(current, "target", profile, binaryName);
      if (exists(candidate)) {
        return candidate;
      }
    }
    const parent = path.dirname(current);
    if (parent === current) {
      return null;
    }
    current = parent;
  }
}

/**
 * Resolve a trusted user-installed executable without inspecting the target
 * repository. Packaged MCP plugins commonly run from an immutable cache that
 * has no `target/` directory, while Codex supplies a restricted PATH that omits
 * Cargo's default bin directory. Falling back to `${CARGO_HOME}/bin` (or
 * `~/.cargo/bin`) keeps the executable source user-owned and avoids running an
 * arbitrary `target/release/leio-code` from the repository being inspected.
 */
export function findInstalledBinary(
  binaryName,
  {
    pathValue = process.env.PATH ?? "",
    cargoHome = process.env.CARGO_HOME ?? "",
    homeDir = os.homedir(),
    cargoTargetDir = process.env.CARGO_TARGET_DIR ?? "",
    exists = fs.existsSync,
  } = {},
) {
  const candidates = [];
  const cargoRoot = cargoHome.trim()
    ? path.resolve(cargoHome)
    : homeDir
      ? path.join(path.resolve(homeDir), ".cargo")
      : null;

  if (cargoRoot) {
    candidates.push(path.join(cargoRoot, "bin", binaryName));
  }

  for (const entry of pathValue.split(path.delimiter)) {
    const trimmed = entry.trim();
    if (trimmed) {
      candidates.push(path.join(path.resolve(trimmed), binaryName));
    }
  }

  for (const candidate of new Set(candidates)) {
    if (isCargoTargetBinary(candidate, { cargoTargetDir })) {
      continue;
    }
    if (exists(candidate)) {
      return candidate;
    }
  }
  return null;
}

/**
 * Resolve `leio-code` for MCP / Apps SDK.
 *
 * Order: `LEIO_CODE_BIN` (explicit regular file) → bundled `vendor/` binary
 * shipped by the package's postinstall → cargo-installed / PATH (never cargo
 * target artifacts) → walk-up `target/` last, stopping at $HOME.
 */
export function resolveBinaryPath({
  binaryName,
  startDir,
  envPath = process.env.LEIO_CODE_BIN ?? "",
  bundledBinaryPath = "",
  pathValue = process.env.PATH ?? "",
  cargoHome = process.env.CARGO_HOME ?? "",
  cargoTargetDir = process.env.CARGO_TARGET_DIR ?? "",
  homeDir = os.homedir(),
  exists = fs.existsSync,
  stat = defaultStat,
} = {}) {
  const trimmed = String(envPath ?? "").trim();
  if (trimmed && isTrustedEnvBinary(trimmed, { exists, stat })) {
    return path.resolve(trimmed);
  }
  const bundled = String(bundledBinaryPath ?? "").trim();
  if (bundled && isTrustedEnvBinary(bundled, { exists, stat })) {
    return path.resolve(bundled);
  }
  if (!binaryName) {
    return null;
  }
  return (
    findInstalledBinary(binaryName, {
      pathValue,
      cargoHome,
      homeDir,
      cargoTargetDir,
      exists,
    }) ??
    (startDir
      ? findBinaryUpwards(startDir, binaryName, exists, { homeDir })
      : null)
  );
}
