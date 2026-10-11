#!/usr/bin/env node
// Build the native processes the Node tests run against. Cheap when they are
// current. The tests start the real binaries: nothing about the loop is faked
// except the application and the models.

import { execFileSync } from 'node:child_process';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const cargo = process.env.CARGO ?? 'cargo';

execFileSync(cargo, ['build', '--quiet', '-p', 'desktop-host', '-p', 'harness-adapter', '-p', 'device-macos', '-p', 'source-macos', '--bins'], {
  cwd: root,
  stdio: 'inherit',
});
const metadata = JSON.parse(
  execFileSync(cargo, ['metadata', '--format-version', '1', '--no-deps'], {
    cwd: root,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
  }),
);
console.log(`native processes are current in ${metadata.target_directory}/debug`);
