import assert from 'node:assert/strict';

export function validateRootLedgerForReuse(evidence, rootId, taskRevision) {
  assert.equal(evidence?.kind, 'host_ledger_evidence', 'Host evidence has an unknown format');
  assert.equal(evidence.scope?.root_filter, 'exact_root_id', 'Host evidence is not Root-scoped');
  assert.equal(evidence.scope?.other_root_rows_exported, false, 'Host evidence includes other Roots');
  assert.equal(evidence.scope?.root_id, rootId, 'Host evidence scope belongs to a different Root');
  assert.equal(evidence.root?.root_id, rootId, 'Host evidence Root row differs from its scope');
  assert.equal(evidence.root?.task_revision, taskRevision, 'Host evidence task revision differs from the observed Root');
  assert.equal(evidence.database?.opened_read_only, true, 'Host evidence database was not opened read-only');
  assert.equal(evidence.database?.query_only, true, 'Host evidence query_only mode is not enabled');
  return evidence;
}

export async function reuseOrExportRootLedger({ rootId, taskRevision, readExisting, exportNew }) {
  assert.equal(typeof rootId, 'string');
  assert(Number.isSafeInteger(taskRevision) && taskRevision >= 0);
  assert.equal(typeof readExisting, 'function');
  assert.equal(typeof exportNew, 'function');

  const existing = await readExisting();
  if (existing !== null && existing !== undefined) {
    return validateRootLedgerForReuse(existing, rootId, taskRevision);
  }

  let exportError = null;
  try { await exportNew(); }
  catch (error) { exportError = error; }

  const created = await readExisting();
  if (created !== null && created !== undefined) {
    return validateRootLedgerForReuse(created, rootId, taskRevision);
  }
  if (exportError) throw exportError;
  throw new Error('Root ledger exporter returned without creating evidence');
}
