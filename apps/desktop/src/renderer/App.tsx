import { useEffect, useMemo, useState } from 'react';

import type { BridgeStopStatus, RunMode, UiProjection, ImageAvailability, NativeAuthorization, OwnedDocumentStatus,
  FrozenTaskCatalogEntry, FrozenTaskCatalogSnapshot, FrozenTaskSelection, ProviderSettingsSnapshot, ConfigureProviderRequest,
  TaskModelSelection } from '@computer-use/desktop-api';

import { FramePreview } from './FramePreview';
import { ObservationPanel } from './ObservationPanel';
import { NativeAuthorizationPanel } from './NativeAuthorizationPanel';
import { ProviderSettingsPanel } from './ProviderSettingsPanel';
import { TaskModelPicker, taskModelSelectionReady } from './TaskModelPicker';
import { OwnedDocumentPanel, hasS2PlanningAuthorization, ownedDocumentBusy, ownedDocumentExists, ownedDocumentPlanningAllowed } from './OwnedDocumentPanel';
import { DesktopShell, type DesktopPage } from './DesktopShell';
import type { ModelAuthorizationStatus } from '@computer-use/desktop-api';
import { toViewModel } from './viewModel';

const MODES: { value: RunMode; label: string }[] = [
  { value: 'observe', label: 'Observe only' },
  { value: 'shadow', label: 'Shadow (suggest, do nothing)' },
  { value: 'execute', label: 'Execute' },
];

const CATALOG_STATUS_MESSAGES: Record<string, string> = {
  configuration_incomplete: 'The frozen task catalog configuration is incomplete.',
  configuration_invalid: 'The frozen task catalog configuration is invalid.',
  catalog_files_unavailable: 'The frozen task catalog files could not be loaded.',
  catalog_files_unsafe: 'The frozen task catalog files are not available for this app.',
  catalog_hash_mismatch: 'The frozen task catalog does not match its configured hashes.',
  catalog_manifest_invalid: 'The frozen task catalog manifest is invalid.',
  catalog_goals_invalid: 'The frozen task records are invalid.',
};

const UNSUPPORTED_SCOPE_MESSAGES: Record<string, string> = {
  form_scope_not_supported: 'This form scope cannot be expressed by the current task interface.',
  list_scope_not_supported: 'This list scope cannot be expressed by the current task interface.',
  modal_scope_not_supported: 'This dialog scope cannot be expressed by the current task interface.',
  textedit_scope_not_supported: 'This TextEdit scope cannot be expressed by the current task interface.',
};

function catalogStatusMessage(reason: string | undefined): string {
  return reason ? CATALOG_STATUS_MESSAGES[reason] ?? 'The frozen task catalog is unavailable.' : 'The frozen task catalog is unavailable.';
}

function unsupportedScopeMessage(reason: string | undefined): string {
  return reason ? UNSUPPORTED_SCOPE_MESSAGES[reason] ?? 'This scope cannot be expressed by the current task interface.'
    : 'This scope cannot be expressed by the current task interface.';
}

export function App() {
  const [projection, setProjection] = useState<UiProjection | null>(null);
  const [bridgeStop, setBridgeStop] = useState<BridgeStopStatus>('idle');
  const [imageAvailability, setImageAvailability] = useState<ImageAvailability | null>(null);
  const [authorization, setAuthorization] = useState<NativeAuthorization | null>(null);
  const [modelAuthorization, setModelAuthorization] = useState<ModelAuthorizationStatus | null>(null);
  const [providerSettings, setProviderSettings] = useState<ProviderSettingsSnapshot | null>(null);
  const [providerSettingsError, setProviderSettingsError] = useState<string | null>(null);
  const [taskModelSelectionPending, setTaskModelSelectionPending] = useState(false);
  const [taskModelSelectionError, setTaskModelSelectionError] = useState<string | null>(null);
  const [ownedDocumentStatus, setOwnedDocumentStatus] = useState<OwnedDocumentStatus | null>(null);
  const [presentation, setPresentation] = useState<'console' | 'indicator'>('console');
  const [text, setText] = useState('');
  const [limitToOneControl, setLimitToOneControl] = useState(false);
  const [allowedApplication, setAllowedApplication] = useState('');
  const [allowedControl, setAllowedControl] = useState('');
  const [allowedGroup, setAllowedGroup] = useState('');
  const [frozenCatalog, setFrozenCatalog] = useState<FrozenTaskCatalogSnapshot | null>(null);
  const [frozenSelection, setFrozenSelection] = useState<FrozenTaskSelection | null>(null);
  const [activePage, setActivePage] = useState<DesktopPage>('tasks');
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let current = true;
    let projected = false, authorizationPublished = false;
    void window.desktop.snapshot().then((snapshot) => {
      if (current && !projected && snapshot) setProjection(snapshot);
    });
    void window.desktop.frozenTaskCatalog().then((catalog) => {
      if (current) setFrozenCatalog(catalog);
    }).catch(() => {
      if (current) setFrozenCatalog({ status: { state: 'unavailable', reason: 'The frozen task catalog could not be loaded.' }, tasks: [] });
    });
    const offProjection = window.desktop.onProjection((value) => { projected = true; setProjection(value); });
    const offStop = window.desktop.onStopStatus(setBridgeStop);
    const offImage = window.desktop.onImageAvailability(setImageAvailability);
    const offAuthorization = window.desktop.onNativeAuthorization((value) => { authorizationPublished = true; setAuthorization(value); });
    void window.desktop.nativeAuthorization().then((status) => { if (current && !authorizationPublished) setAuthorization(status); });
    let presented = false;
    const offPresentation = window.desktop.onPresentation((value) => { presented = true; setPresentation(value); });
    void window.desktop.presentation().then((value) => { if (current && !presented) setPresentation(value); });
    let modelPublished = false;
    const offModel = window.desktop.onModelAuthorization((value) => { modelPublished = true; setModelAuthorization(value); });
    void window.desktop.modelAuthorization().then((value) => { if (current && !modelPublished) setModelAuthorization(value); });
    let providerSettingsPublished = false;
    const offProviderSettings = window.desktop.onProviderSettings((value) => {
      providerSettingsPublished = true;
      setProviderSettings(value);
      setProviderSettingsError(null);
    });
    void window.desktop.providerSettings().then((value) => {
      if (current && !providerSettingsPublished) {
        setProviderSettings(value);
        setProviderSettingsError(null);
      }
    }).catch(() => {
      if (current && !providerSettingsPublished) setProviderSettingsError('Provider settings could not be loaded.');
    });
    let ownedDocumentPublished = false;
    const offOwnedDocument = window.desktop.onOwnedDocumentStatus((value) => {
      ownedDocumentPublished = true;
      setOwnedDocumentStatus(value);
    });
    void window.desktop.ownedDocumentStatus().then((value) => {
      if (current && !ownedDocumentPublished) setOwnedDocumentStatus(value);
    });
    return () => {
      current = false;
      offProjection();
      offStop();
      offImage();
      offAuthorization();
      offPresentation();
      offModel();
      offProviderSettings();
      offOwnedDocument();
    };
  }, []);

  const view = useMemo(() => toViewModel(projection, bridgeStop), [projection, bridgeStop]);
  const changingSession = authorization?.phase === 'authorizing' || authorization?.phase === 'switching' || authorization?.phase === 'unavailable'
    || modelAuthorization?.phase === 'authorizing' || modelAuthorization?.phase === 'switching'
    || ownedDocumentBusy(ownedDocumentStatus) || providerSettings?.busy === true;
  const hasOwnedDocument = ownedDocumentExists(ownedDocumentStatus);
  const ownedPlanningAllowed = ownedDocumentPlanningAllowed(ownedDocumentStatus, modelAuthorization);
  const taskEntryDisabled = changingSession || !view.canCreate || !ownedPlanningAllowed || frozenCatalog === null
    || providerSettings === null || providerSettings.busy || taskModelSelectionPending
    || !taskModelSelectionReady(providerSettings, modelAuthorization);
  const explicitDemoAvailable = modelAuthorization?.phase === 'not_configured'
    && modelAuthorization.route === null && modelAuthorization.credential_kind === null
    && modelAuthorization.planning_available;
  const taskModelChangeDisabled = changingSession || view.canStop
    || providerSettings?.busy === true || taskModelSelectionPending;
  const frozenMode = frozenCatalog !== null && frozenCatalog.status.state !== 'empty';
  const scopeInputInvalid = limitToOneControl && (allowedApplication.trim() === '' || allowedControl.trim() === '');
  const selectedFrozenTask = frozenSelection && frozenCatalog?.tasks.find((task) =>
    task.catalog_id === frozenSelection.catalog_id && task.selector === frozenSelection.selector && task.scope_compatible);
  const canSelectBrowserPageProfile = view.canSelectBrowserPageProfile
    && authorization?.phase === 'not_native'
    && modelAuthorization?.phase === 'not_configured'
    && ownedDocumentStatus?.phase === 'not_created'
    && !changingSession;

  // A refused or failed call is shown, not swallowed.
  const run = (call: () => Promise<void>): void => {
    setError(null);
    call().catch((failure: unknown) => setError(failure instanceof Error ? failure.message : String(failure)));
  };

  const selectTaskModel = (selection: TaskModelSelection): void => {
    if (taskModelChangeDisabled) return;
    setTaskModelSelectionPending(true);
    setTaskModelSelectionError(null);
    void window.desktop.selectTaskModel(selection).catch(() => {
      setTaskModelSelectionError('Review the connection and choose a task model again.');
    }).finally(() => setTaskModelSelectionPending(false));
  };

  if (presentation === 'indicator') return <main className="execution-indicator">
    <div role="status"><strong>LingXi Control · {view.stateLabel}</strong></div>
    <button type="button" className="stop" onClick={() => run(() => window.desktop.stop())}>Stop</button>
    <span>⌘⇧Esc · Stop from the tray</span>
    {error ? <span role="alert">{error}</span> : null}
  </main>;

  return (
    <DesktopShell
      activePage={activePage}
      onNavigate={setActivePage}
      toolbar={(
        <header className="drag-region desktop-topbar">
          <div className="desktop-topbar-project">
            <span className="desktop-topbar-project-icon" aria-hidden="true">
              <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round">
                <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
              </svg>
            </span>
            <div className="desktop-topbar-title">{view.capability}</div>
          </div>
          <label className="mode no-drag">
            <span>Mode</span>
            <select value={view.mode} disabled={changingSession} onChange={(event) => run(() => window.desktop.setMode(event.target.value as RunMode))}>
              {MODES.map((mode) => (
                <option key={mode.value} value={mode.value} disabled={mode.value === 'execute'
                  && (!view.canExecute || !ownedPlanningAllowed || (authorization?.phase !== 'not_native' && authorization?.phase !== 'authorized'))}>
                  {mode.label}
                </option>
              ))}
            </select>
          </label>
          {canSelectBrowserPageProfile ? <button className="no-drag" type="button" disabled={changingSession}
            onClick={() => run(() => window.desktop.selectBrowserPageProfile())}>
            Use fixture browser
          </button> : null}
          <span className="desktop-runtime-state" role="status">{view.stateLabel}</span>
          <button type="button" className="stop no-drag desktop-topbar-stop" disabled={!view.canStop && !changingSession}
            onClick={() => run(() => window.desktop.stop())}>
            Stop
          </button>
        </header>
      )}
      footer={<><span className="sidebar-footer-label">Session</span><span role="status">{view.stateLabel}</span></>}
    >
      {error && <div className="banner warning" role="alert">{error}</div>}
      {view.stop.level !== 'none' && <div className={`banner ${view.stop.level}`} role="status">{view.stop.text}</div>}

      {view.prompt && (
        <section className="panel prompt" role="alertdialog" aria-labelledby="prompt-title">
          <h2 id="prompt-title">{view.prompt.title}</h2>
          <ul>
            {view.prompt.lines.map((line) => <li key={line}>{line}</li>)}
          </ul>
          <div className="row">
            <button type="button" className="primary" disabled={changingSession || !ownedPlanningAllowed}
              onClick={() => run(() => window.desktop.confirm(view.prompt!.id, true))}>
              {view.prompt.kind === 'goal' ? 'Yes, do it' : 'Allow this action'}
            </button>
            <button type="button" disabled={changingSession || !ownedPlanningAllowed}
              onClick={() => run(() => window.desktop.confirm(view.prompt!.id, false))}>
              No
            </button>
          </div>
        </section>
      )}

      <section className="workspace-page" aria-label="Tasks" hidden={activePage !== 'tasks'}>
        <header className="workspace-intro">
          <div className="workspace-heading">
            <h1>What would you like to do?</h1>
            <p>Describe the outcome you want. The app checks the target and asks for confirmation before a task runs.</p>
          </div>
        </header>
        <section className="panel task-composer">
          <TaskModelPicker snapshot={providerSettings}
            authorization={modelAuthorization}
            demoAvailable={explicitDemoAvailable === true}
            disabled={taskModelChangeDisabled}
            pending={taskModelSelectionPending}
            error={taskModelSelectionError}
            loadError={providerSettingsError}
            onSelect={selectTaskModel} />
          {frozenCatalog === null ? <>
            <h1>Loading task options</h1>
            <p role="status">Checking the configured task catalog.</p>
          </> : frozenMode ? <>
            <h1>Choose a frozen task</h1>
            <p>Submit the selected task. The app checks the target window and goal, may ask you to authorize the window and confirm the task, and may refuse it.</p>
            {frozenCatalog.status.state === 'unavailable' ? <p role="alert">
              {catalogStatusMessage(frozenCatalog.status.reason)}
            </p> : null}
            {frozenCatalog.tasks.length > 0 ? <ol aria-label="Frozen task catalog">
              {frozenCatalog.tasks.map((task, index) => <FrozenTaskOption key={`${task.catalog_id}:${task.selector}`}
                task={task}
                index={index}
                selected={selectedFrozenTask?.catalog_id === task.catalog_id && selectedFrozenTask.selector === task.selector}
                disabled={taskEntryDisabled}
                onSelect={() => setFrozenSelection({ catalog_id: task.catalog_id, selector: task.selector })} />)}
            </ol> : <p>No task records are available.</p>}
            <button type="button" className="primary"
              disabled={taskEntryDisabled || !selectedFrozenTask || modelAuthorization?.planning_available === false}
              onClick={() => {
                const selected = selectedFrozenTask;
                if (!selected) return;
                run(() => window.desktop.createFrozenTask({ catalog_id: selected.catalog_id, selector: selected.selector }));
                setFrozenSelection(null);
              }}>
              Submit selected task
            </button>
          </> : <>
            <textarea
              aria-label="Task"
              rows={3}
              disabled={taskEntryDisabled}
              value={text}
              placeholder="Describe the task and the result you want"
              onChange={(event) => setText(event.target.value)}
            />
            <label>
              <input aria-label="Limit task to one control" type="checkbox" checked={limitToOneControl}
                disabled={taskEntryDisabled} onChange={(event) => setLimitToOneControl(event.target.checked)} />
              Limit this task to one control
            </label>
            {limitToOneControl ? <fieldset className="scope-fields">
              <legend>Allowed scope</legend>
              <p>Use this for a simple on/off control in a supported background app.</p>
              <label>Application
                <input aria-label="Allowed application" value={allowedApplication} disabled={taskEntryDisabled}
                  onChange={(event) => setAllowedApplication(event.target.value)} />
              </label>
              <label>Control
                <input aria-label="Allowed control" value={allowedControl} disabled={taskEntryDisabled}
                  onChange={(event) => setAllowedControl(event.target.value)} />
              </label>
              <label>Group (optional)
                <input aria-label="Allowed group" value={allowedGroup} disabled={taskEntryDisabled}
                  onChange={(event) => setAllowedGroup(event.target.value)} />
              </label>
            </fieldset> : null}
            <button
              type="button"
              className="primary"
              disabled={taskEntryDisabled || scopeInputInvalid || text.trim() === '' || modelAuthorization?.planning_available === false}
              onClick={() => {
                if (limitToOneControl) {
                  const group = allowedGroup.trim() ? allowedGroup : undefined;
                  run(() => window.desktop.createScopedTask({
                    objective: text,
                    allowed_scope: {
                      application: allowedApplication,
                      targets: [{ control: allowedControl, ...(group !== undefined ? { group } : {}) }],
                      no_unrelated_controls: true,
                    },
                  }));
                } else {
                  run(() => window.desktop.createTask(text.trim()));
                }
                setText('');
              }}
            >
              Start
            </button>
          </>}
        </section>
        {view.rootTaskId ? <section className="panel" aria-live="polite">
          {view.headline && <div className="headline">{view.headline}</div>}
          {view.progress && <progress max={view.progress.max} value={view.progress.used} aria-label="Attempts used" />}
          <div className="row">
            <button type="button" disabled={changingSession || !view.canPause} onClick={() => run(() => window.desktop.pause(view.rootTaskId!))}>Pause</button>
            <button type="button" disabled={changingSession || !view.canResume} onClick={() => run(() => window.desktop.resume(view.rootTaskId!))}>Resume</button>
          </div>
        </section> : null}
      </section>

      <section className="workspace-page" aria-label="Model providers" hidden={activePage !== 'connection'}>
        <header className="workspace-intro"><div className="workspace-heading"><h1>Model providers</h1><p>Configure provider access here. Choose the model for each new task in Tasks.</p></div></header>
        <ProviderSettingsPanel snapshot={providerSettings}
          loadError={providerSettingsError}
          configure={(request: ConfigureProviderRequest) => window.desktop.configureProvider(request)}
          refreshModels={(id) => window.desktop.refreshProviderModels(id)}
          removeConnection={(id) => window.desktop.removeProvider(id)}
          cancelSignIn={() => window.desktop.stop()} />
      </section>

      <section className="workspace-page" aria-label="Workspace" hidden={activePage !== 'workspace'}>
        <header className="workspace-intro"><div className="workspace-heading"><h1>Workspace</h1><p>Manage the target app and the document this session may use.</p></div></header>
        {authorization && (!hasOwnedDocument || ownedDocumentStatus?.phase === 'not_created') ? <NativeAuthorizationPanel status={authorization}
          authorize={(id) => run(() => window.desktop.authorizeNativeWindow(id))}
          refresh={() => run(() => window.desktop.refreshNativeObservation())}
          revoke={() => run(() => window.desktop.revokeNativeWindow())} /> : null}
        {ownedDocumentStatus ? <OwnedDocumentPanel status={ownedDocumentStatus}
          planningAuthorized={hasS2PlanningAuthorization(modelAuthorization)}
          create={() => window.desktop.createOwnedDocument()}
          authorizeEdit={() => window.desktop.authorizeOwnedDocumentEdit()}
          authorizeSave={() => window.desktop.authorizeOwnedDocumentSave()}
          revisit={() => window.desktop.revisitOwnedDocument()}
          revoke={() => window.desktop.revokeNativeWindow()} /> : null}
        {!authorization && !ownedDocumentStatus ? <section className="panel" role="status">Loading workspace authorization.</section> : null}
      </section>

      <section className="workspace-page" aria-label="Observation" hidden={activePage !== 'observation'}>
        <header className="workspace-intro"><div className="workspace-heading"><h1>Observation</h1><p>Review the current read-only view of the target.</p></div></header>
        {view.observation ? <>
          <ObservationPanel observation={view.observation} />
          <FramePreview frame={projection?.observation?.frame} availability={imageAvailability} />
        </> : null}
        {view.browserObservation ? <section className="panel" aria-label="Fixture browser observation">
          <h2>Fixture browser</h2>
          <p>Background option: {view.browserObservation.checked === null
            ? 'state not observed' : view.browserObservation.checked ? 'on' : 'off'}</p>
          <p>Read only until you confirm a task and choose Execute.</p>
        </section> : null}
        {view.timeline.length > 0 ? <section className="panel timeline" aria-labelledby="activity-heading">
          <h2 id="activity-heading">Activity</h2>
          <ol>{view.timeline.map((entry, index) => <li key={`${index}-${entry}`}>{entry}</li>)}</ol>
        </section> : null}
      </section>
    </DesktopShell>
  );
}

function FrozenTaskOption({ task, index, selected, disabled, onSelect }: {
  task: FrozenTaskCatalogEntry;
  index: number;
  selected: boolean;
  disabled: boolean;
  onSelect: () => void;
}) {
  const scopeLabel: Record<FrozenTaskCatalogEntry['scope_family'], string> = {
    control: 'Single control', form: 'Form', list: 'List', modal: 'Dialog', textedit: 'TextEdit document',
  };
  return <li>
    <button type="button" aria-pressed={selected} disabled={disabled || !task.scope_compatible} onClick={onSelect}>
      Frozen task {index + 1} · {scopeLabel[task.scope_family]}
    </button>
    {!task.scope_compatible ? <p>{unsupportedScopeMessage(task.unsupported_reason)}</p> : null}
  </li>;
}
