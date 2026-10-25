import { join } from 'node:path';
import { mkdirSync } from 'node:fs';

interface BrandApplication {
  getName(): string;
  getPath(name: 'appData' | 'userData'): string;
  setName(name: string): void;
  setPath(name: 'userData', path: string): void;
}

/** Display branding must not change the directory holding accounts and tasks. */
export function configureBrandIdentity(app: BrandApplication): void {
  if (app.getPath('userData') === join(app.getPath('appData'), app.getName())) {
    const userData = join(app.getPath('appData'), 'Computer Use');
    // Electron setPath requires an existing directory, including on first launch.
    mkdirSync(userData, { recursive: true });
    app.setPath('userData', userData);
  }
  // Preserve explicitly configured userData directories used by diagnostics.
  app.setName('LingXi Control');
}
