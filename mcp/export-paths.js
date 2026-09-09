import fs from "node:fs";
import path from "node:path";

function tryRealpath(candidate) {
  try {
    return fs.realpathSync(candidate);
  } catch {
    return null;
  }
}

/**
 * Resolve `candidate` and reject paths that escape `repoRoot`.
 */
export function confineToRepo(repoRoot, candidate) {
  if (!candidate) {
    return null;
  }
  const root = path.resolve(repoRoot);
  const realRoot = tryRealpath(root) ?? root;
  const resolved = path.resolve(root, candidate);
  const realExisting = tryRealpath(resolved);
  const confined = realExisting
    ?? path.join(tryRealpath(path.dirname(resolved)) ?? path.dirname(resolved), path.basename(resolved));
  const rel = path.relative(realRoot, confined);
  if (rel.startsWith("..") || path.isAbsolute(rel)) {
    throw new Error(`path must stay inside repo_root: ${candidate}`);
  }
  return resolved;
}
