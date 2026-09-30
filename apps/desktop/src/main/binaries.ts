import { existsSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export interface Binaries {
  host: string;
  device: string;
  source: string;
}

/**
 * Where the native processes are. A packaged app carries them in its
 * resources; a development checkout has them in the Cargo target directory;
 * `CU_BIN_DIR` overrides both for tests and scripts.
 */
export function resolveBinaries(env: NodeJS.ProcessEnv = process.env, resourcesPath?: string): Binaries {
  const here = dirname(fileURLToPath(import.meta.url));
  const candidates = [
    env['CU_BIN_DIR'],
    resourcesPath ? resolve(resourcesPath, 'bin') : undefined,
    resolve(here, '../../../../target/debug'),
    resolve(here, '../../../../target/release'),
  ].filter((dir): dir is string => Boolean(dir));
  for (const dir of candidates) {
    const binaries = {
      host: resolve(dir, 'cu-host'),
      device: resolve(dir, 'cu-device'),
      source: resolve(dir, 'cu-source'),
    };
    if (Object.values(binaries).every((path) => existsSync(path))) return binaries;
  }
  throw new Error(`the native processes were not found; looked in: ${candidates.join(', ') || '(nowhere)'}`);
}
