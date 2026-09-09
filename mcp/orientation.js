import fs from 'node:fs';
import path from 'node:path';

/** Snapshot only recorded coverage; index age never proves source freshness. */
export function indexDiagnostics(repoRoot, indexPath, now = Date.now()) {
  const artifact = indexPath || path.join(repoRoot, '.leio-code/index.json');
  try {
    const index = JSON.parse(fs.readFileSync(artifact, 'utf8'));
    if (!Array.isArray(index.files) || typeof index.root !== 'string') throw new Error('Invalid index shape');
    const canonical = value => { try { return fs.realpathSync(value); } catch { return path.resolve(value); } };
    const matches = canonical(index.root) === canonical(repoRoot);
    const languages = {};
    for (const file of index.files) languages[file.language || 'unknown'] = (languages[file.language || 'unknown'] || 0) + 1;
    const timestamp = Date.parse(index.indexed_at);
    return {
      state: matches ? 'available' : 'repository_mismatch', path: artifact,
      repository: index.root, schema_version: index.version,
      indexed_at: index.indexed_at ?? null,
      age_seconds: Number.isFinite(timestamp) ? Math.max(0, Math.floor((now - timestamp) / 1000)) : null,
      indexed_files: index.files.length, indexed_languages: languages,
      source_freshness: 'not_checked', excluded_files: null,
      limitations: ['Indexed languages describe observed files, not all supported languages.',
        'Excluded files and call-edge completeness are not measured by this snapshot.',
        ...(!matches ? ['This index belongs to a different repository; do not rely on its results.'] : [])],
    };
  } catch (error) {
    return { state: error.code === 'ENOENT' ? 'missing' : 'unreadable', path: artifact,
      source_freshness: 'unknown', excluded_files: null,
      limitations: ['Index coverage is unavailable. Run leio_code_index for the selected repository.'] };
  }
}

export function retrievalAssessment(envelope) {
  const files = envelope?.entities?.[0]?.files_to_read ?? [];
  return { state: files.length ? 'ranked_candidates' : 'no_matches',
    selected_files: files.length, calibrated_confidence: false,
    limitations: ['Ranking scores are retrieval hints, not verified relevance or architecture evidence.'],
    next_action: files.length ? 'Read the top candidate, then inspect its exact graph definitions.'
      : 'Inspect index coverage and retry with an exact path or identifier; do not infer architecture from this result.' };
}
