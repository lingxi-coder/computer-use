import type { DesktopApi } from '@computer-user/desktop-api';

declare global {
  interface Window {
    /** Exposed by the preload script; the only way the page reaches main. */
    desktop: DesktopApi;
  }
}

export {};
