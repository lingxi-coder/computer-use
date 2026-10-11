import type { DesktopApi } from '@computer-use/desktop-api';

declare global {
  interface Window {
    /** Exposed by the preload script; the only way the page reaches main. */
    desktop: DesktopApi;
  }
}

export {};
