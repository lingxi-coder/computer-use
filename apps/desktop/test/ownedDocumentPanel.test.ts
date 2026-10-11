import assert from 'node:assert/strict';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { test } from 'node:test';
import type { ModelAuthorizationStatus, OwnedDocumentStatus } from '@computer-use/desktop-api';
import {
  OwnedDocumentPanel,
  hasS2PlanningAuthorization,
  ownedDocumentBusy,
  ownedDocumentPlanningAllowed,
} from '../src/renderer/OwnedDocumentPanel.js';

const actions = {
  create: async () => {}, authorizeEdit: async () => {}, authorizeSave: async () => {},
  revisit: async () => {}, revoke: async () => {},
};

function buttonDisabled(markup: string, label: string): boolean {
  const end = markup.indexOf(`>${label}</button>`);
  assert.notEqual(end, -1, `button ${label} exists`);
  const start = markup.lastIndexOf('<button', end);
  return /\sdisabled(?:="")?/.test(markup.slice(start, end));
}

function status(change: Partial<OwnedDocumentStatus> = {}): OwnedDocumentStatus {
  return {
    phase: 'not_created', can_create: false, can_authorize_edit: false,
    can_authorize_save: false, can_revisit: false, reason: null, ...change,
  };
}

function model(purposes: ModelAuthorizationStatus['authorized_purposes'], phase: ModelAuthorizationStatus['phase'] = 'authorized'): ModelAuthorizationStatus {
  return {
    phase, route: null, credential_kind: null, storage: null, authorized_purposes: purposes, can_authorize: false,
    can_save: false, planning_available: false, saved: [], reason: null,
  };
}

test('temporary-document panel disables unavailable actions and keeps edit, Save and reopen as separate steps', () => {
  const html = renderToStaticMarkup(createElement(OwnedDocumentPanel, { status: status({
    phase: 'observing', can_create: true, can_authorize_edit: true,
  }), planningAuthorized: false, ...actions }));
  assert.equal(buttonDisabled(html, 'Create temporary document'), false);
  assert.equal(buttonDisabled(html, 'Allow editing…'), false);
  assert.equal(buttonDisabled(html, 'Allow saving…'), true);
  assert.equal(buttonDisabled(html, 'Reopen to verify'), true);
  assert.match(html, /separate native permission/);
  assert.match(html, /does not select or edit an existing document/);
  assert.match(html, /Approve editing and saving .* before starting or confirming a task or enabling Execute/);
  assert.match(html, /configure and authorize an S2 planning model/);
  assert.doesNotMatch(html, /document\.txt|[a-f0-9]{64}|private body/i);

  const noRegistration = renderToStaticMarkup(createElement(OwnedDocumentPanel, { status: status({
    can_authorize_save: true, can_revisit: true,
  }), planningAuthorized: true, ...actions }));
  assert.equal(buttonDisabled(noRegistration, 'Allow saving…'), true);
  assert.equal(buttonDisabled(noRegistration, 'Reopen to verify'), true);
});

test('Save needs S2 planning and an explicit current save authorization; busy revisit locks all steps', () => {
  const authorized = renderToStaticMarkup(createElement(OwnedDocumentPanel, { status: status({
    phase: 'edit_authorized', can_authorize_save: true,
  }), planningAuthorized: true, ...actions }));
  assert.equal(buttonDisabled(authorized, 'Allow saving…'), false);
  assert.equal(buttonDisabled(authorized, 'Reopen to verify'), true);

  const revisiting = renderToStaticMarkup(createElement(OwnedDocumentPanel, { status: status({
    phase: 'revisiting', can_create: true, can_authorize_edit: true,
    can_authorize_save: true, can_revisit: true,
  }), planningAuthorized: true, ...actions }));
  for (const label of ['Create temporary document', 'Allow editing…', 'Allow saving…', 'Reopen to verify']) {
    assert.equal(buttonDisabled(revisiting, label), true, label);
  }
  assert.match(revisiting, /Reopening the temporary document for verification/);

  const awaitingPermission = renderToStaticMarkup(createElement(OwnedDocumentPanel, { status: status({
    phase: 'authorizing_edit',
  }), planningAuthorized: false, ...actions }));
  assert.equal(buttonDisabled(awaitingPermission, 'Cancel permission request'), false);
});

test('owned task and Execute gates require Save authorization and a live S2 purpose', () => {
  const editAuthorized = status({ phase: 'edit_authorized' });
  const saveAuthorized = status({ phase: 'save_authorized' });
  assert.equal(ownedDocumentPlanningAllowed(null, null), true);
  assert.equal(ownedDocumentPlanningAllowed(editAuthorized, model(['s2_planning'])), false);
  assert.equal(ownedDocumentPlanningAllowed(saveAuthorized, null), false);
  assert.equal(ownedDocumentPlanningAllowed(saveAuthorized, model(['s1_teacher'])), false);
  assert.equal(ownedDocumentPlanningAllowed(saveAuthorized, model(['s2_planning'], 'switching')), false);
  assert.equal(ownedDocumentPlanningAllowed(saveAuthorized, model(['s2_planning'])), true);
  assert.equal(hasS2PlanningAuthorization(model(['s2_planning'])), true);
  assert.equal(ownedDocumentBusy(status({ phase: 'authorizing_edit' })), true);
  assert.equal(ownedDocumentBusy(status({ phase: 'authorizing_save' })), true);
});
