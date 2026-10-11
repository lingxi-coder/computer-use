import { existsSync, lstatSync, realpathSync } from 'node:fs';
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
 * `CU_BIN_DIR` overrides development paths for tests and scripts. Installed
 * apps accept only their own regular executable resources.
 */
export function resolveBinaries(env: NodeJS.ProcessEnv = process.env, resourcesPath?: string, isPackaged = false): Binaries {
  const here = dirname(fileURLToPath(import.meta.url));
  const candidates = (isPackaged ? [resourcesPath ? resolve(resourcesPath, 'bin') : undefined] : [
    env['CU_BIN_DIR'],
    resourcesPath ? resolve(resourcesPath, 'bin') : undefined,
    resolve(here, '../../../../target/debug'),
    resolve(here, '../../../../target/release'),
  ]).filter((dir): dir is string => Boolean(dir));
  for (const dir of candidates) {
    const binaries = {
      host: resolve(dir, 'cu-host'),
      device: resolve(dir, 'cu-device'),
      source: resolve(dir, 'cu-source'),
    };
    if (Object.values(binaries).every((path) => isPackaged ? isPackagedExecutable(path) : existsSync(path))) return binaries;
  }
  throw new Error(`the native processes were not found; looked in: ${candidates.join(', ') || '(nowhere)'}`);
}

function isPackagedExecutable(path: string): boolean {
  try {
    const stat = lstatSync(path);
    return stat.isFile() && !stat.isSymbolicLink() && (stat.mode & 0o111) !== 0 && realpathSync(path) === path;
  } catch {
    return false;
  }
}
