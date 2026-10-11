#!/usr/bin/env node
import { lstatSync, mkdirSync, readFileSync } from 'node:fs';
import { isAbsolute, resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { MAX_ENVIRONMENT_JSON_BYTES, buildTaskProfileBundle, writeFrozenTaskProfiles } from './task-profiles.mjs';

function readManifest(path) {
  if (!isAbsolute(path)) throw new Error('manifest path must be absolute');
  const stat = lstatSync(path);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size <= 0 || stat.size > MAX_ENVIRONMENT_JSON_BYTES) {
    throw new Error('manifest file is not a bounded regular file');
  }
  return JSON.parse(readFileSync(path, 'utf8'));
}

function main() {
  const { values } = parseArgs({ options: {
    environment: { type: 'string' },
    'task-output-dir': { type: 'string' },
    'out-dir': { type: 'string' },
  } });
  if (!values.environment || !values['task-output-dir'] || !values['out-dir']) {
    throw new Error('usage: node scripts/mvp/freeze-task-profiles.mjs --environment /absolute/environment.json --task-output-dir /absolute/task-output --out-dir /absolute/freeze-output');
  }
  for (const [label, value] of [['environment', values.environment], ['task-output-dir', values['task-output-dir']], ['out-dir', values['out-dir']]]) {
    if (!isAbsolute(value)) throw new Error(`${label} must be an absolute path`);
  }
  const environment = readManifest(resolve(values.environment));
  const bundle = buildTaskProfileBundle({ environment, taskOutputDirectory: resolve(values['task-output-dir']) });
  const outputDirectory = resolve(values['out-dir']);
  mkdirSync(outputDirectory, { recursive: true, mode: 0o700 });
  const written = writeFrozenTaskProfiles(outputDirectory, bundle);
  process.stdout.write(`${JSON.stringify({
    status: 'profile_plan_frozen',
    execution_started: false,
    business_results: 'not_run',
    denominators: bundle.freezeManifest.denominators,
    hashes: written.hashes,
    outputs: written.files,
    goal_catalog_outputs: written.goalCatalogFiles,
    goal_catalog_hashes: written.goalCatalogHashes,
    catalog_id: written.catalogId,
  })}\n`);
}

try { main(); }
catch {
  process.stderr.write('Task-profile freeze rejected. Check required environment fields, bounded asset paths and hashes, and choose an unused output directory.\n');
  process.exitCode = 1;
}
