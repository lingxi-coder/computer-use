import { join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

import { app, BrowserWindow, dialog, globalShortcut, ipcMain, Menu, nativeImage, screen, session, shell, Tray } from 'electron';
import type { UiProjection } from '@computer-use/desktop-api';

import { DecisionWorker } from '@computer-use/decision-worker';

import { configureBrandIdentity } from './brandIdentity.js';
import { Channels } from '../shared/channels.js';
import { resolveBinaries } from './binaries.js';
import { DecisionRelay } from './decisionRelay.js';
import { createSessionDecisionBackend } from './decisionBackend.js';
import { LocalDecisionProcess } from './localDecisionProcess.js';
import { TeacherShadowObserver } from './teacherShadowObserver.js';
import { HostBridge } from './hostBridge.js';
import { registerIpc, type IpcEventLike, type TrustedOrigins } from './ipc.js';
import { nativeSourceFromEnvironment } from './nativeSource.js';
import { NativeSession } from './nativeSession.js';
import { contentSecurityPolicyForUrl, developmentRendererUrl } from './rendererSecurity.js';
import { StopBridge } from './stopBridge.js';
import { hideWindowOnClose } from './windowLifecycle.js';
import { executionPresentationTransition, indicatorBounds } from './executionPresentation.js';
import { ModelAccess } from './modelAccess.js';
import { ProviderSettings } from './providerSettings.js';
import { createProviderCatalog } from './providerCatalog.js';
import { createOpenAiLogin, createOpenAiOAuthRefresher } from './openAiLogin.js';
import { NativeKeychain, ProductModelKeyStore } from './modelKeyStore.js';
import { Supervisor } from './supervisor.js';
import { createOwnedDocumentLaunchRunner } from './ownedDocumentLaunch.js';
import { fixtureBackgroundOptionSupervisorOptions } from './browserProfile.js';
import { browserDiagnostics } from './browserDiagnostics.js';
import { loadFrozenTaskCatalog } from './frozenTaskCatalog.js';

const moduleDirectory = dirname(fileURLToPath(import.meta.url));
configureBrandIdentity(app);
const brandIconPath = join(app.getAppPath(), 'assets', 'brand', 'icon.png');
// A 16 by 16 red square: the stop control's mark in the tray, made here so no binary asset ships with it.
const TRAY_ICON = 'iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAAGUlEQVR42mM4ISf3nxLMMGrAqAGjBgwXAwAG2gMfkOrhWAAAAABJRU5ErkJggg==';

let window: BrowserWindow | null = null;
let tray: Tray | null = null;
let showWindow: (() => void) | null = null;
let supervisor: NativeSession | null = null;
let models: ModelAccess | null = null;
let providers: ProviderSettings | null = null;
let stopBridge: StopBridge | null = null;
let quitting = false;
let presentation: 'console' | 'indicator' = 'console';
let consoleBounds: Electron.Rectangle | null = null;

function presentExecution(projection: UiProjection): void {
  if (!window || window.isDestroyed()) return;
  const executing = supervisor?.authorization.phase === 'authorized' && supervisor.authorization.authorized !== null && projection.mode === 'execute'
    && projection.root != null && ['ready', 'running', 'waiting_evidence', 'waiting_plan', 'needs_revalidation'].includes(projection.root.state);
  const transition = executionPresentationTransition(executing, supervisor?.authorization.authorized_domain ?? null, presentation);
  if (transition === 'restore_console') {
    presentation = 'console'; window.setMinimumSize(780, 560); window.setAlwaysOnTop(false); window.setFocusable(true); window.setResizable(true);
    if (consoleBounds) window.setBounds(consoleBounds); window.webContents.send(Channels.presentationChanged, presentation);
    window.showInactive();
    return;
  }
  if (transition === 'keep_console') return;
  if (transition === 'enter_indicator') {
    consoleBounds = window.getBounds(); window.hide(); presentation = 'indicator';
    window.setMinimumSize(0, 0); window.setFocusable(false); window.setResizable(false); window.setAlwaysOnTop(true, 'floating');
    window.webContents.send(Channels.presentationChanged, presentation);
  }
  const target = projection.observation?.observed_target;
  const allowed = supervisor?.authorization.authorized;
  const points = target && target.pid === allowed?.pid && target.window_id === allowed.window_id ? target.frame_points : null;
  const bounds = indicatorBounds(points, screen.getAllDisplays().map((d) => d.workArea));
  if (bounds) { const current = window.getBounds(); if (Object.keys(bounds).some((k) => bounds[k as keyof typeof bounds] !== current[k as keyof typeof current])) window.setBounds(bounds); window.showInactive(); }
  else window.hide();
}

function createWindow(devUrl: string | undefined): BrowserWindow {
  const created = new BrowserWindow({
    width: 1180,
    height: 820,
    minWidth: 780,
    minHeight: 560,
    titleBarStyle: 'hiddenInset',
    show: false,
    title: 'LingXi Control',
    icon: brandIconPath,
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
  hideWindowOnClose(created, () => quitting);
  created.once('closed', () => {
    if (window === created) window = null;
  });
  created.once('ready-to-show', () => created.show());
  // The window shows what main sends it and goes nowhere else.
  created.webContents.on('will-navigate', (event) => event.preventDefault());
  created.webContents.setWindowOpenHandler(() => ({ action: 'deny' }));
  if (devUrl) void created.loadURL(devUrl);
  else void created.loadFile(join(moduleDirectory, '../renderer/index.html'));
  return created;
}

function secureSession(devUrl: string | undefined): void {
  session.defaultSession.setPermissionRequestHandler((_webContents, _permission, callback) => callback(false));
  session.defaultSession.webRequest.onHeadersReceived((details, callback) => {
    callback({
      responseHeaders: {
        ...details.responseHeaders,
        'Content-Security-Policy': [contentSecurityPolicyForUrl(details.url, devUrl)],
      },
    });
  });
}

function createTray(stop: StopBridge, showWindow: () => void): Tray {
  const tray = new Tray(nativeImage.createFromBuffer(Buffer.from(TRAY_ICON, 'base64')));
  tray.setToolTip('LingXi Control');
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
  app.dock?.setIcon(brandIconPath);
  const devUrl = developmentRendererUrl(process.env['ELECTRON_RENDERER_URL'], app.isPackaged);
  const frozenTaskCatalog = loadFrozenTaskCatalog(process.env);
  secureSession(devUrl);
  const binaries = resolveBinaries(process.env, process.resourcesPath, app.isPackaged);
  const nativeSource = nativeSourceFromEnvironment(process.env, binaries);
  const openAiLogin = createOpenAiLogin({
    helperPath: join(dirname(binaries.host), 'cu-auth'),
    openExternal: (url) => shell.openExternal(url),
  });
  const refreshOpenAiOAuth = createOpenAiOAuthRefresher({ helperPath: join(dirname(binaries.host), 'cu-auth') });
  const keyStore = new ProductModelKeyStore(join(app.getPath('userData'), 'model-access'), new NativeKeychain(join(dirname(binaries.host), 'cu-macos-keychain')));
  // The trusted Connect action and its selected purpose checkboxes are the
  // user's authorization. Keep Stop cancellation, without asking twice.
  models = new ModelAccess(async (_route, _storage, _purposes, signal) => !signal.aborted, keyStore,
  !nativeSource || nativeSource.referencePlanner === true, openAiLogin);
  await models.initialize();
  supervisor = await NativeSession.start({
    binaries,
    ...browserDiagnostics,
    confirmComputerSafety: async (request, signal) => {
      if (!window || window.isDestroyed() || signal.aborted) return false;
      const target = supervisor?.authorization.authorized;
      if (!target || models?.authorization.phase !== 'authorized'
        || !models.authorization.authorized_purposes.includes('computer_proposals')) return false;
      const checks = request.pending_safety_checks.map((check) => check.message ?? check.code ?? check.id);
      const actions = request.proposal?.actions.map((action, index) => `${index + 1}. ${JSON.stringify(action)}`).join('\n');
      const detail = `${target.window_title ?? 'Untitled window'}\n${target.app}\n\n${checks.join('\n\n')}${actions ? `\n\nProposed actions:\n${actions}` : '\n\nThe model requests a screenshot of this window.'}`;
      if (detail.length > 16_384) return false;
      const result = await dialog.showMessageBox(window, {
        type: 'question', title: 'Review the requested action',
        message: 'Allow this request in the selected window?', detail,
        buttons: ['Cancel', 'Allow this request'], defaultId: 0, cancelId: 0, noLink: true, signal,
      });
      return result.response === 1 && !signal.aborted;
    },
    teacherObservationFactory: (nowMs) => {
      const revision = 'e4e9ddf21a7b1903b7acffd8814ad4307bf63a67';
      const modelRoot = app.isPackaged ? join(process.resourcesPath, 'models', 'laya-multilingual', revision)
        : resolve(moduleDirectory, '../../../..', 'target/model-exports/laya-multilingual', revision);
      const local = LocalDecisionProcess.open({
        workerModulePath: join(moduleDirectory, 'localDecisionWorker.js'),
        bundleManifestPath: join(modelRoot, 'package-manifest.json'), numThreads: 2, nowMs,
      });
      return new TeacherShadowObserver(local, { nowMs });
    },
    ...(nativeSource ? { nativeSource } : {}),
    // The app starts observing. Execution is something the person turns on.
    mode: 'observe',
  }, async (target, domain) => {
    const backgroundAx = domain === 'background_ax';
    const options = {
      type: 'question' as const,
      title: backgroundAx ? 'Allow background AXPress?' : 'Allow this window?',
      message: backgroundAx
        ? 'Allow LingXi Control to press verified controls in this background window?'
        : 'Allow LingXi Control to read ordinary text and edit registered controls in this window?',
      detail: backgroundAx
        ? `${target.window_title ?? 'Untitled window'}\n${target.app}\nProcess ${target.pid} · ${target.window_id}\n\nThis permission is bound to this process identity and background window until you revoke it or quit. It allows verified AXPress actions. Tasks still require separate goal confirmation and Execute mode. Revoking permission closes this input session.`
        : `${target.window_title ?? 'Untitled window'}\n${target.app}\nProcess ${target.pid} · ${target.window_id}\n\nThis permission lasts until you revoke it or quit. Tasks still require goal confirmation and Execute mode. Revoking permission cancels the current input session.`,
      buttons: ['Cancel', backgroundAx ? 'Allow background AXPress' : 'Allow this window'], defaultId: 0, cancelId: 0, noLink: true,
    };
    const result = window && !window.isDestroyed() ? await dialog.showMessageBox(window, options) : await dialog.showMessageBox(options);
    return result.response === 1;
  }, (options) => Supervisor.start(models!.supervisorOptions(options)), Date.now, {
    createRunner: createOwnedDocumentLaunchRunner,
    canPlan: () => models?.authorization.phase === 'authorized'
      && models.authorization.authorized_purposes.includes('s2_planning'),
    confirmEdit: async (signal) => {
      if (!window || window.isDestroyed()) return false;
      return (await dialog.showMessageBox(window, {
        type: 'question', title: 'Allow editing the temporary document?',
        message: 'Allow LingXi Control to edit its temporary TextEdit document?',
        detail: 'This permission applies only to the new TextEdit document created by LingXi Control. Task confirmation and Execute mode are still required. Saving has a separate permission. You can stop or revoke editing at any time.',
        buttons: ['Cancel', 'Allow editing'], defaultId: 0, cancelId: 0, noLink: true, signal,
      })).response === 1;
    },
    confirmSave: async (signal) => {
      if (!window || window.isDestroyed()) return false;
      return (await dialog.showMessageBox(window, {
        type: 'question', title: 'Allow saving the temporary document?',
        message: 'Allow LingXi Control to save and reopen its temporary TextEdit document?',
        detail: 'This permission applies only to the temporary document created by LingXi Control. After the task saves it, LingXi Control may close that TextEdit instance and reopen the same file to verify the result. Reopening returns to observation and does not retain editing or saving permission. The temporary document is removed when the session is cleaned up; export or permanent-file tasks are not enabled by this permission.',
        buttons: ['Cancel', 'Allow saving and reopening'], defaultId: 0, cancelId: 0, noLink: true, signal,
      })).response === 1;
    },
  });
  models.attach(supervisor);
  const providerCatalog = createProviderCatalog(join(dirname(binaries.host), 'cu-auth'));
  // Provider Settings is reached through the trusted Connect action too; the
  // selected purposes are the request, and only Stop interrupts configuration.
  providers = new ProviderSettings(models, keyStore, providerCatalog.load, providerCatalog.refresh,
    async (provider, signal) => {
      const model = provider.models.find((entry) => entry.selectable)?.id;
      if (!model) throw new Error('The provider has no supported task models.');
      return openAiLogin({ model }, signal);
    },
    async (_provider, _storage, _purposes, signal) => !signal.aborted, refreshOpenAiOAuth);
  await providers.initialize();
  supervisor.on('log', (line) => console.error(line));
  supervisor.on('exit', (role, code, signal) => console.error(`${role} exited (${code ?? signal})`));

  const makeDecisionWorker = () => new DecisionWorker(createSessionDecisionBackend(() =>
    supervisor ? { native: supervisor.authorization.phase, owned: supervisor.ownedDocumentStatus.phase } : null));
  const relay = new DecisionRelay(supervisor, makeDecisionWorker());
  relay.attach();
  supervisor.on('sessionChanged', () => {
    relay.replaceWorker(makeDecisionWorker());
    stopBridge?.resetSession();
  });

  stopBridge = new StopBridge(supervisor, {
    register: (accelerator, callback) => globalShortcut.register(accelerator, callback),
    unregister: (accelerator) => globalShortcut.unregister(accelerator),
  });
  if (!stopBridge.start()) console.error('the global stop hotkey could not be registered; the button and the tray still work');
  if (supervisor.projection) stopBridge.observeProjection(supervisor.projection);

  window = createWindow(devUrl);
  showWindow = () => {
    if (!window || window.isDestroyed()) window = createWindow(devUrl);
    else if (presentation === 'indicator') window.showInactive();
    else window.show();
  };
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
    new HostBridge(supervisor, stopBridge, supervisor, () => presentation, models, async () => {
      if (!supervisor) throw new Error('The desktop session is unavailable.');
      const browserOptions = fixtureBackgroundOptionSupervisorOptions({
        isPackaged: app.isPackaged,
        resourcesPath: process.resourcesPath,
        moduleDirectory,
        platform: process.platform,
      });
      await supervisor.startBrowserProfile(browserOptions);
    }, providers),
    trusted,
    frozenTaskCatalog,
  );
  supervisor.on('projection', (projection) => {
    presentExecution(projection);
    stopBridge?.observeProjection(projection);
    if (window && !window.isDestroyed()) window.webContents.send(Channels.projection, projection);
  });
  supervisor.on('imageAvailability', (available) => {
    if (window && !window.isDestroyed()) window.webContents.send(Channels.imageAvailability, available);
  });
  supervisor.on('nativeAuthorization', (status) => {
    if (window && !window.isDestroyed()) window.webContents.send(Channels.nativeAuthorizationChanged, status);
  });
  supervisor.on('ownedDocumentStatus', (status) => {
    if (window && !window.isDestroyed()) window.webContents.send(Channels.ownedDocumentStatusChanged, status);
  });
  providers.on('status', (status) => {
    if (window && !window.isDestroyed()) window.webContents.send(Channels.providerSettingsChanged, status);
  });
  models.on('status', (status) => {
    if (window && !window.isDestroyed()) window.webContents.send(Channels.modelAuthorizationChanged, status);
  });
  stopBridge.on('status', (status) => {
    if (window && !window.isDestroyed()) window.webContents.send(Channels.stopStatus, status);
  });
  tray = createTray(stopBridge, () => showWindow?.());
}

async function shutdown(): Promise<void> {
  stopBridge?.dispose();
  globalShortcut.unregisterAll();
  await providers?.shutdown();
  if (models) await models.shutdown(async () => { await supervisor?.shutdown(); });
  else await supervisor?.shutdown();
  tray?.destroy();
  tray = null;
}

if (!app.requestSingleInstanceLock()) {
  app.quit();
} else {
  app.on('second-instance', () => showWindow?.());
  app.on('activate', () => showWindow?.());
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
