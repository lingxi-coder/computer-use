import { existsSync, lstatSync, realpathSync } from 'node:fs';
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';

export const FIXTURE_BACKGROUND_OPTION_PROFILE = { kind: 'fixture_background_option' } as const;
const FIXTURE_PARTS = ['fixtures', 'headless', 'readonly-page.html'] as const;
const CHROME_PARTS = ['Google Chrome.app', 'Contents', 'MacOS', 'Google Chrome'] as const;
const MAX_FIXTURE_BYTES = 1_048_576;

export interface BrowserProfileRuntimePaths {
  readonly isPackaged: boolean;
  readonly resourcesPath: string;
  readonly moduleDirectory: string;
  readonly platform: NodeJS.Platform;
  readonly homeDirectory: string;
  readonly exists: (path: string) => boolean;
}

export interface FixtureBackgroundOptionSupervisorOptions {
  browserPageProfile: typeof FIXTURE_BACKGROUND_OPTION_PROFILE;
  browserSource: { chromeExecutable: string; fixturePath: string };
}

function requireFixedFixture(path: string): void {
  try {
    const stat = lstatSync(path);
    if (!stat.isFile() || stat.isSymbolicLink() || stat.size <= 0 || stat.size > MAX_FIXTURE_BYTES
      || realpathSync(path) !== path) throw new Error('fixture is not a regular canonical asset');
  } catch {
    throw new Error('The fixed fixture browser page is missing or is not a regular canonical asset.');
  }
}

/** Resolves only the fixed Main-owned fixture and a standard installed Chrome path. */
export function fixtureBackgroundOptionSupervisorOptions(
  runtime: Partial<BrowserProfileRuntimePaths> & Pick<BrowserProfileRuntimePaths, 'isPackaged' | 'resourcesPath' | 'moduleDirectory'>,
): FixtureBackgroundOptionSupervisorOptions {
  const platform = runtime.platform ?? process.platform;
  if (platform !== 'darwin') throw new Error('The fixed fixture browser profile is available only on macOS.');

  const homeDirectory = runtime.homeDirectory ?? homedir();
  const exists = runtime.exists ?? existsSync;
  const chromeCandidates = [
    join('/Applications', ...CHROME_PARTS),
    join(homeDirectory, 'Applications', ...CHROME_PARTS),
  ];
  const chromeExecutable = chromeCandidates.find(exists);
  if (!chromeExecutable) throw new Error('Install Google Chrome in /Applications or ~/Applications to use this fixture.');

  const fixturePath = runtime.isPackaged
    ? join(runtime.resourcesPath, ...FIXTURE_PARTS)
    : resolve(runtime.moduleDirectory, '../../../../', ...FIXTURE_PARTS);
  requireFixedFixture(fixturePath);
  return {
    browserPageProfile: FIXTURE_BACKGROUND_OPTION_PROFILE,
    browserSource: { chromeExecutable, fixturePath },
  };
}
