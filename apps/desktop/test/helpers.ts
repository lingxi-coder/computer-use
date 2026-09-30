import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { resolveBinaries, type Binaries } from '../src/main/binaries.js';
import type { Supervisor } from '../src/main/supervisor.js';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');

let cached: Binaries | null = null;

/** The real native processes, built if they are not current. */
export function binaries(): Binaries {
  if (cached) return cached;
  const cargo = process.env['CARGO'] ?? 'cargo';
  execFileSync(cargo, ['build', '--quiet', '-p', 'desktop-host', '-p', 'device-macos', '-p', 'source-macos', '--bins'], {
    cwd: root,
    stdio: 'inherit',
  });
  const metadata = JSON.parse(
    execFileSync(cargo, ['metadata', '--format-version', '1', '--no-deps'], {
      cwd: root,
      encoding: 'utf8',
      maxBuffer: 64 * 1024 * 1024,
    }),
  ) as { target_directory: string };
  cached = resolveBinaries({ ...process.env, CU_BIN_DIR: resolve(metadata.target_directory, 'debug') });
  return cached;
}

/** Wait until something holds, or fail with what the processes said. */
export async function until(
  done: () => boolean,
  what: string,
  logs: string[] = [],
  timeoutMs = 20_000,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!done()) {
    if (Date.now() > deadline) {
      throw new Error(`timed out waiting for ${what}\n${logs.slice(-30).join('\n')}`);
    }
    await new Promise((resolveWait) => setTimeout(resolveWait, 10));
  }
}

/** Collect what the processes write to stderr, for a failing test's message. */
export function collectLogs(supervisor: Supervisor): string[] {
  const lines: string[] = [];
  supervisor.on('log', (line) => lines.push(line));
  return lines;
}

/** The inputs that have reached the simulated application, from its world log. */
export function worldInputs(supervisor: Supervisor): string[] {
  if (!existsSync(supervisor.worldLogPath)) return [];
  return readFileSync(supervisor.worldLogPath, 'utf8')
    .split('\n')
    .filter((line) => line.includes('"kind":"input"'));
}

export const TEXT = 'Turn off "Daily picks" in "Push"';
export const TEXT_AMBIGUOUS = 'Turn off "Daily picks"';

export function alive(pid: number | undefined): boolean {
  if (pid === undefined) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}
