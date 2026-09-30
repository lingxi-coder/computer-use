import { join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

import { app, BrowserWindow, globalShortcut, ipcMain, Menu, nativeImage, session, Tray } from 'electron';

import { DecisionWorker, ScriptedTeacher } from '@computer-user/decision-worker';

import { Channels } from '../shared/channels.js';
import { resolveBinaries } from './binaries.js';
import { DecisionRelay } from './decisionRelay.js';
import { HostBridge } from './hostBridge.js';
import { registerIpc, type IpcEventLike, type TrustedOrigins } from './ipc.js';
import { StopBridge } from './stopBridge.js';
import { Supervisor } from './supervisor.js';

const moduleDirectory = dirname(fileURLToPath(import.meta.url));
// A 16 by 16 red square: the stop control's mark in the tray, made here so no binary asset ships with it.
const TRAY_ICON = 'iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAAGUlEQVR42mM4ISf3nxLMMGrAqAGjBgwXAwAG2gMfkOrhWAAAAABJRU5ErkJggg==';

const CONTENT_SECURITY_POLICY = [
  "default-src 'self'",
  "script-src 'self'",
  "style-src 'self'",
  "img-src 'self' data:",
  "connect-src 'none'",
  "object-src 'none'",
  "base-uri 'none'",
  "form-action 'none'",
].join('; ');

function developmentRendererUrl(): string | undefined {
  if (app.isPackaged) return undefined;
  const raw = process.env['ELECTRON_RENDERER_URL'];
  if (!raw) return undefined;
  try {
    const url = new URL(raw);
    const loopback = url.hostname === 'localhost' || url.hostname === '127.0.0.1' || url.hostname === '::1';
    if ((url.protocol === 'http:' || url.protocol === 'https:') && loopback) return url.toString();
  } catch {
    // not a URL: no dev server
  }
  return undefined;
}

let window: BrowserWindow | null = null;
let supervisor: Supervisor | null = null;
let stopBridge: StopBridge | null = null;
let quitting = false;

function createWindow(devUrl: string | undefined): BrowserWindow {
  const created = new BrowserWindow({
    width: 440,
    height: 720,
    show: false,
    title: 'Computer User',
    webPreferences: {
      preload: join(moduleDirectory, '../preload/index.cjs'),
      // The renderer is a web page: no Node, isolated from the preload, sandboxed.
      sandbox: true,
      contextIsolation: true,
      nodeIntegration: false,
      webSecurity: true,
      allowRunningInsecureContent: false,
      spellcheck: false,
    },
  });
  created.once('ready-to-show', () => created.show());
  // The window shows what main sends it and goes nowhere else.
  created.webContents.on('will-navigate', (event) => event.preventDefault());
  created.webContents.setWindowOpenHandler(() => ({ action: 'deny' }));
  if (devUrl) void created.loadURL(devUrl);
  else void created.loadFile(join(moduleDirectory, '../renderer/index.html'));
  return created;
}

function secureSession(): void {
  session.defaultSession.setPermissionRequestHandler((_webContents, _permission, callback) => callback(false));
  session.defaultSession.webRequest.onHeadersReceived((details, callback) => {
    callback({
      responseHeaders: { ...details.responseHeaders, 'Content-Security-Policy': [CONTENT_SECURITY_POLICY] },
    });
  });
}

function createTray(stop: StopBridge, showWindow: () => void): Tray {
  const tray = new Tray(nativeImage.createFromBuffer(Buffer.from(TRAY_ICON, 'base64')));
  tray.setToolTip('Computer User');
  tray.setContextMenu(
    Menu.buildFromTemplate([
      // The same path as the button and the hotkey: the device first.
      { label: 'Stop', click: () => stop.press('tray') },
      { label: 'Show window', click: showWindow },
      { type: 'separator' },
      { label: 'Quit', click: () => app.quit() },
    ]),
  );
  return tray;
}

async function main(): Promise<void> {
  secureSession();
  const devUrl = developmentRendererUrl();
  supervisor = await Supervisor.start({
    binaries: resolveBinaries(process.env, process.resourcesPath),
    // The app starts observing. Execution is something the person turns on.
    mode: 'observe',
  });
  supervisor.on('log', (line) => console.error(line));
  supervisor.on('exit', (role, code, signal) => console.error(`${role} exited (${code ?? signal})`));

  // The decision worker is scripted in this build. The real one runs in a
  // utilityProcess and is relayed through main exactly like this.
  new DecisionRelay(supervisor, new DecisionWorker(new ScriptedTeacher())).attach();

  stopBridge = new StopBridge(supervisor, {
    register: (accelerator, callback) => globalShortcut.register(accelerator, callback),
    unregister: (accelerator) => globalShortcut.unregister(accelerator),
  });
  if (!stopBridge.start()) console.error('the global stop hotkey could not be registered; the button and the tray still work');

  window = createWindow(devUrl);
  const trusted: TrustedOrigins = {
    files: [resolve(moduleDirectory, '../renderer/index.html')],
    ...(devUrl ? { devOrigin: devUrl } : {}),
  };
  registerIpc(
    {
      handle: (channel, listener) =>
        ipcMain.handle(channel, (event, ...args) =>
          listener({ senderFrame: event.senderFrame ? { url: event.senderFrame.url } : null } satisfies IpcEventLike, ...args),
        ),
      removeHandler: (channel) => ipcMain.removeHandler(channel),
    },
    new HostBridge(supervisor, stopBridge),
    trusted,
  );
  supervisor.on('projection', (projection) => window?.webContents.send(Channels.projection, projection));
  stopBridge.on('status', (status) => window?.webContents.send(Channels.stopStatus, status));
  createTray(stopBridge, () => window?.show());
}

async function shutdown(): Promise<void> {
  stopBridge?.dispose();
  globalShortcut.unregisterAll();
  await supervisor?.shutdown();
}

if (!app.requestSingleInstanceLock()) {
  app.quit();
} else {
  app.on('second-instance', () => window?.show());
  app.on('window-all-closed', () => undefined); // the tray keeps the stop control alive
  app.on('before-quit', (event) => {
    if (quitting) return;
    event.preventDefault();
    quitting = true;
    void shutdown().finally(() => app.exit(0));
  });
  void app
    .whenReady()
    .then(main)
    .catch((error: unknown) => {
      console.error('could not start:', error);
      app.exit(1);
    });
}
