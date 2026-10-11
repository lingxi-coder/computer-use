import assert from 'node:assert/strict';
import test from 'node:test';
import { reuseOrExportRootLedger } from './root-ledger-reuse.mjs';

function evidence(rootId = 'root-1', taskRevision = 3) {
  return {
    kind: 'host_ledger_evidence',
    scope: { root_filter: 'exact_root_id', other_root_rows_exported: false, root_id: rootId },
    root: { root_id: rootId, task_revision: taskRevision },
    database: { opened_read_only: true, query_only: true },
  };
}

test('reuses existing read-only evidence only when its exact Root and revision match', async () => {
  let exports = 0;
  const existing = evidence();
  const result = await reuseOrExportRootLedger({
    rootId: 'root-1', taskRevision: 3,
    readExisting: async () => existing,
    exportNew: async () => { exports += 1; },
  });
  assert.equal(result, existing);
  assert.equal(exports, 0);
});

test('does not overwrite evidence for another Root or revision', async () => {
  for (const wrong of [evidence('root-2', 3), evidence('root-1', 4)]) {
    let exports = 0;
    await assert.rejects(reuseOrExportRootLedger({
      rootId: 'root-1', taskRevision: 3,
      readExisting: async () => wrong,
      exportNew: async () => { exports += 1; },
    }));
    assert.equal(exports, 0);
  }
});

test('exports once when absent, then validates the created exact-Root evidence', async () => {
  let stored = null;
  let exports = 0;
  const result = await reuseOrExportRootLedger({
    rootId: 'root-1', taskRevision: 3,
    readExisting: async () => stored,
    exportNew: async () => { exports += 1; stored = evidence(); },
  });
  assert.equal(result, stored);
  assert.equal(exports, 1);
});

test('accepts a racing exact-Root file after the exporter reports an existing-output error', async () => {
  let stored = null;
  let exports = 0;
  const result = await reuseOrExportRootLedger({
    rootId: 'root-1', taskRevision: 3,
    readExisting: async () => stored,
    exportNew: async () => { exports += 1; stored = evidence(); throw new Error('output already exists'); },
  });
  assert.equal(result, stored);
  assert.equal(exports, 1);
});
